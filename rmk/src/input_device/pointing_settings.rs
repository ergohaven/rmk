//! Key actions for the persistent pointing settings exposed by a board.

use rmk_types::action::Action;

use crate::config::VialDeviceSettings;
use crate::event::ActionEvent;

/// Handle nine consecutive User actions: toggle/on/off for both, left, then right.
///
/// `first_id` is the first appended custom keycode in the board's definition.
/// Boards with one pointer can expose only the first three actions. Setting
/// callbacks publish the live update; persistence uses the host's usual queue.
pub fn handle_sticky_mode_key(event: ActionEvent, settings: VialDeviceSettings<'_>, first_id: u8) {
    let Action::User(id) = event.action else { return };
    let Some(offset) = id.checked_sub(first_id).filter(|&offset| offset < 9) else {
        return;
    };
    if !event.keyboard_event.pressed {
        return;
    }

    let scope = offset / 3;
    let selected = [scope != 2, scope != 1];
    let current: [Option<bool>; 2] = core::array::from_fn(|side| {
        let mut value = [0];
        selected[side]
            .then(|| (settings.get_setting)(140 + side as u16, &mut value))
            .flatten()
            .map(|_| value[0] != 0)
    });
    let all_enabled = current.iter().flatten().all(|&enabled| enabled);
    let enabled = offset % 3 == 1 || (offset % 3 == 0 && !all_enabled);
    let mut changed = false;
    for (side, old) in current.into_iter().enumerate() {
        if old.is_some_and(|old| old != enabled) {
            changed |= (settings.set_setting)(140 + side as u16, &[u8::from(enabled)]);
        }
    }
    if changed {
        #[cfg(feature = "storage")]
        crate::storage::queue_device_settings_flash_write((settings.serialize)());
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU8, Ordering};

    use rmk_types::action::Action;
    use rmk_types::keycode::HidKeyCode;

    use super::*;
    use crate::config::{VialDeviceSettings, VialDeviceSettingsData};
    use crate::event::{ActionEvent, KeyboardEvent};

    static FLAGS: AtomicU8 = AtomicU8::new(0);
    static WRITES: AtomicU8 = AtomicU8::new(0);

    fn get(id: u16, out: &mut [u8]) -> Option<usize> {
        let bit = match id {
            140 => 6,
            141 => 7,
            _ => return None,
        };
        out[0] = (FLAGS.load(Ordering::Relaxed) >> bit) & 1;
        Some(1)
    }

    fn set(id: u16, value: &[u8]) -> bool {
        let mask = match id {
            140 => 0x40,
            141 => 0x80,
            _ => return false,
        };
        if value[0] == 0 {
            FLAGS.fetch_and(!mask, Ordering::Relaxed);
        } else {
            FLAGS.fetch_or(mask, Ordering::Relaxed);
        }
        WRITES.fetch_add(1, Ordering::Relaxed);
        true
    }

    fn api() -> VialDeviceSettings<'static> {
        VialDeviceSettings {
            setting_keys: &[140, 141],
            get_setting: get,
            set_setting: set,
            serialize: || VialDeviceSettingsData::empty(),
            deserialize: |_| {},
        }
    }

    fn event(id: u8, pressed: bool) -> ActionEvent {
        ActionEvent {
            action: Action::User(id),
            keyboard_event: KeyboardEvent::keycode(HidKeyCode::A, pressed),
        }
    }

    #[test]
    fn sticky_keys_toggle_mixed_sides_together_and_ignore_release() {
        FLAGS.store(0x55, Ordering::Relaxed);
        WRITES.store(0, Ordering::Relaxed);
        handle_sticky_mode_key(event(41, true), api(), 41);
        assert_eq!(FLAGS.load(Ordering::Relaxed), 0xd5);
        handle_sticky_mode_key(event(41, false), api(), 41);
        assert_eq!(FLAGS.load(Ordering::Relaxed), 0xd5);
        handle_sticky_mode_key(event(41, true), api(), 41);
        assert_eq!(FLAGS.load(Ordering::Relaxed), 0x15);
        assert_eq!(WRITES.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn sticky_keys_explicit_on_off_are_idempotent() {
        FLAGS.store(0x15, Ordering::Relaxed);
        WRITES.store(0, Ordering::Relaxed);
        for _ in 0..2 {
            handle_sticky_mode_key(event(42, true), api(), 41);
        }
        assert_eq!(FLAGS.load(Ordering::Relaxed), 0xd5);
        assert_eq!(WRITES.load(Ordering::Relaxed), 2);
        for _ in 0..2 {
            handle_sticky_mode_key(event(43, true), api(), 41);
        }
        assert_eq!(FLAGS.load(Ordering::Relaxed), 0x15);
        assert_eq!(WRITES.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn sticky_keys_side_actions_preserve_the_other_side() {
        FLAGS.store(0x15, Ordering::Relaxed);
        handle_sticky_mode_key(event(45, true), api(), 41);
        assert_eq!(FLAGS.load(Ordering::Relaxed), 0x55);
        handle_sticky_mode_key(event(48, true), api(), 41);
        assert_eq!(FLAGS.load(Ordering::Relaxed), 0xd5);
        handle_sticky_mode_key(event(46, true), api(), 41);
        assert_eq!(FLAGS.load(Ordering::Relaxed), 0x95);
        handle_sticky_mode_key(event(47, true), api(), 41);
        assert_eq!(FLAGS.load(Ordering::Relaxed), 0x15);
    }

    #[test]
    fn sticky_keys_support_a_single_right_pointer_setting() {
        FLAGS.store(0x15, Ordering::Relaxed);
        let mut settings = api();
        settings.get_setting = |id, out| if id == 141 { get(id, out) } else { None };
        handle_sticky_mode_key(event(13, true), settings, 13);
        assert_eq!(FLAGS.load(Ordering::Relaxed), 0x95);
        handle_sticky_mode_key(event(15, true), settings, 13);
        assert_eq!(FLAGS.load(Ordering::Relaxed), 0x15);
    }

    #[test]
    fn sticky_keys_ignore_unrelated_actions_and_unavailable_settings() {
        FLAGS.store(0x55, Ordering::Relaxed);
        WRITES.store(0, Ordering::Relaxed);
        for id in [0, 40, 50, 255] {
            handle_sticky_mode_key(event(id, true), api(), 41);
        }
        let mut action = event(41, true);
        action.action = Action::No;
        handle_sticky_mode_key(action, api(), 41);
        let mut settings = api();
        settings.get_setting = |_, _| None;
        handle_sticky_mode_key(event(41, true), settings, 41);
        assert_eq!(FLAGS.load(Ordering::Relaxed), 0x55);
        assert_eq!(WRITES.load(Ordering::Relaxed), 0);
    }
}
