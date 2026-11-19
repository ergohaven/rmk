//! QMK-compatible runtime key remapping (the `QK_MAGIC` keycodes).
//!
//! QMK keeps these switches in its keymap configuration.  RMK stores the same
//! state separately from the keymap so a host can change it without rewriting
//! any layout entries.  The VIA wire keycodes remain the upstream QMK values;
//! only the internal [`Action::User`](rmk_types::action::Action::User) ids are
//! RMK-owned.

use rmk_types::action::{Action, KeyAction};
use rmk_types::keycode::HidKeyCode;
use rmk_types::modifier::ModifierCombination;

/// First QMK magic keycode.
pub const KEYCODE_START: u16 = 0x7000;
/// Last QMK magic keycode in the current QMK range.
pub const KEYCODE_END: u16 = 0x7022;
/// First RMK user id reserved for QMK magic actions.
pub const USER_ID_START: u8 = 0xC0;

/// Reserved legacy user-data slot for the persisted Magic configuration.
///
/// This must remain a [`StorageKey::UserData`](crate::storage::StorageKey) slot:
/// adding a new serialized key/value enum variant would change the wire tags
/// seen by older firmware. The slot is intentionally outside normal board
/// user-data ranges; writes preserve any pre-existing non-Magic content.
pub(crate) const MAGIC_STORAGE_SLOT: u8 = u8::MAX;

const MAGIC_STORAGE_TAG: [u8; 4] = *b"RMKM";
const MAGIC_STORAGE_VERSION: u8 = 1;
const MAGIC_STORAGE_PAYLOAD_LEN: usize = MAGIC_STORAGE_TAG.len() + 3;

const MAGIC_OFFSET_END: u8 = (KEYCODE_END - KEYCODE_START) as u8;

/// QMK magic actions supported by RMK.
///
/// The discriminants deliberately match the offset from `QK_MAGIC` where
/// possible.  The three NKRO actions and the two EE Hands actions are left out
/// until their device-specific implementations are available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MagicAction {
    SwapControlCapsLock = 0x00,
    UnswapControlCapsLock = 0x01,
    ToggleControlCapsLock = 0x02,
    CapsLockAsControlOff = 0x03,
    CapsLockAsControlOn = 0x04,
    SwapLAltLGui = 0x05,
    UnswapLAltLGui = 0x06,
    SwapRAltRGui = 0x07,
    UnswapRAltRGui = 0x08,
    GuiOn = 0x09,
    GuiOff = 0x0A,
    ToggleGui = 0x0B,
    SwapGraveEscape = 0x0C,
    UnswapGraveEscape = 0x0D,
    SwapBackslashBackspace = 0x0E,
    UnswapBackslashBackspace = 0x0F,
    ToggleBackslashBackspace = 0x10,
    SwapAltGui = 0x14,
    UnswapAltGui = 0x15,
    ToggleAltGui = 0x16,
    SwapLCtrlLGui = 0x17,
    UnswapLCtrlLGui = 0x18,
    SwapRCtrlRGui = 0x19,
    UnswapRCtrlRGui = 0x1A,
    SwapCtrlGui = 0x1B,
    UnswapCtrlGui = 0x1C,
    ToggleCtrlGui = 0x1D,
    SwapEscapeCapsLock = 0x20,
    UnswapEscapeCapsLock = 0x21,
    ToggleEscapeCapsLock = 0x22,
}

impl MagicAction {
    /// The QMK `QK_MAGIC_*` keycode represented by this action.
    pub const fn keycode(self) -> u16 {
        KEYCODE_START + self as u16
    }

    /// The RMK `Action::User` id used to carry this action through keymaps and
    /// the existing native action codec.
    pub const fn user_id(self) -> u8 {
        USER_ID_START + self as u8
    }

    /// Decode a supported QMK magic keycode.
    pub const fn from_keycode(keycode: u16) -> Option<Self> {
        if keycode < KEYCODE_START || keycode > KEYCODE_END {
            return None;
        }
        Self::from_offset((keycode - KEYCODE_START) as u8)
    }

    /// Decode a supported RMK user id.
    pub const fn from_user_id(user_id: u8) -> Option<Self> {
        if user_id < USER_ID_START || user_id > USER_ID_START + MAGIC_OFFSET_END {
            return None;
        }
        Self::from_offset(user_id - USER_ID_START)
    }

    const fn from_offset(offset: u8) -> Option<Self> {
        Some(match offset {
            0x00 => Self::SwapControlCapsLock,
            0x01 => Self::UnswapControlCapsLock,
            0x02 => Self::ToggleControlCapsLock,
            0x03 => Self::CapsLockAsControlOff,
            0x04 => Self::CapsLockAsControlOn,
            0x05 => Self::SwapLAltLGui,
            0x06 => Self::UnswapLAltLGui,
            0x07 => Self::SwapRAltRGui,
            0x08 => Self::UnswapRAltRGui,
            0x09 => Self::GuiOn,
            0x0A => Self::GuiOff,
            0x0B => Self::ToggleGui,
            0x0C => Self::SwapGraveEscape,
            0x0D => Self::UnswapGraveEscape,
            0x0E => Self::SwapBackslashBackspace,
            0x0F => Self::UnswapBackslashBackspace,
            0x10 => Self::ToggleBackslashBackspace,
            0x14 => Self::SwapAltGui,
            0x15 => Self::UnswapAltGui,
            0x16 => Self::ToggleAltGui,
            0x17 => Self::SwapLCtrlLGui,
            0x18 => Self::UnswapLCtrlLGui,
            0x19 => Self::SwapRCtrlRGui,
            0x1A => Self::UnswapRCtrlRGui,
            0x1B => Self::SwapCtrlGui,
            0x1C => Self::UnswapCtrlGui,
            0x1D => Self::ToggleCtrlGui,
            0x20 => Self::SwapEscapeCapsLock,
            0x21 => Self::UnswapEscapeCapsLock,
            0x22 => Self::ToggleEscapeCapsLock,
            _ => return None,
        })
    }
}

/// Return the QMK magic capability bitmap.
///
/// Bit `n` corresponds to QMK keycode `0x7000 + n`.  The return type covers
/// all 35 QMK magic ids while leaving unsupported actions clear.
pub const fn supported_keycodes() -> u64 {
    let mut bitmap = 0u64;
    let mut offset = 0u8;
    while offset <= MAGIC_OFFSET_END {
        if MagicAction::from_offset(offset).is_some() {
            bitmap |= 1u64 << offset;
        }
        offset += 1;
    }
    bitmap
}

/// Return the same bitmap as the five little-endian bytes used by the Vial
/// native capability report.
pub const fn supported_keycode_bytes() -> [u8; 5] {
    let bitmap = supported_keycodes().to_le_bytes();
    [bitmap[0], bitmap[1], bitmap[2], bitmap[3], bitmap[4]]
}

/// Whether an internal action is valid for a native host action write.
///
/// The reserved RMK user-id range is deliberately closed: generic user ids
/// outside it remain available to existing firmware extensions, while values
/// inside it may only carry a supported QMK magic action.
pub(crate) fn action_is_supported(action: Action) -> bool {
    match action {
        Action::User(id) if (USER_ID_START..=USER_ID_START + MAGIC_OFFSET_END).contains(&id) => {
            MagicAction::from_user_id(id).is_some()
        }
        _ => true,
    }
}

/// Validate every action nested in a native key action before mutation.
pub(crate) fn key_action_is_supported(key_action: KeyAction) -> bool {
    match key_action {
        KeyAction::Single(action) | KeyAction::Tap(action) => action_is_supported(action),
        KeyAction::TapHold(tap, hold, _) => action_is_supported(tap) && action_is_supported(hold),
        KeyAction::No | KeyAction::Transparent | KeyAction::Morse(_) => true,
        _ => false,
    }
}

/// Persistent QMK magic switches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct MagicConfig {
    pub swap_control_capslock: bool,
    pub capslock_to_control: bool,
    pub swap_lalt_lgui: bool,
    pub swap_ralt_rgui: bool,
    pub no_gui: bool,
    pub swap_grave_esc: bool,
    pub swap_backslash_backspace: bool,
    pub swap_lctl_lgui: bool,
    pub swap_rctl_rgui: bool,
    pub swap_escape_capslock: bool,
}

impl MagicConfig {
    /// Encode the persisted representation without changing the storage map's
    /// existing serialized enum discriminants.
    pub(crate) const fn to_storage_payload(self) -> [u8; MAGIC_STORAGE_PAYLOAD_LEN] {
        let mut flags = 0u16;
        if self.swap_control_capslock {
            flags |= 1 << 0;
        }
        if self.capslock_to_control {
            flags |= 1 << 1;
        }
        if self.swap_lalt_lgui {
            flags |= 1 << 2;
        }
        if self.swap_ralt_rgui {
            flags |= 1 << 3;
        }
        if self.no_gui {
            flags |= 1 << 4;
        }
        if self.swap_grave_esc {
            flags |= 1 << 5;
        }
        if self.swap_backslash_backspace {
            flags |= 1 << 6;
        }
        if self.swap_lctl_lgui {
            flags |= 1 << 7;
        }
        if self.swap_rctl_rgui {
            flags |= 1 << 8;
        }
        if self.swap_escape_capslock {
            flags |= 1 << 9;
        }

        [
            MAGIC_STORAGE_TAG[0],
            MAGIC_STORAGE_TAG[1],
            MAGIC_STORAGE_TAG[2],
            MAGIC_STORAGE_TAG[3],
            MAGIC_STORAGE_VERSION,
            flags as u8,
            (flags >> 8) as u8,
        ]
    }

    /// Decode a persisted Magic payload, rejecting unknown tags, versions, or
    /// future flag bits so arbitrary legacy user data is never treated as ours.
    pub(crate) const fn from_storage_payload(data: &[u8]) -> Option<Self> {
        if data.len() != MAGIC_STORAGE_PAYLOAD_LEN
            || data[0] != MAGIC_STORAGE_TAG[0]
            || data[1] != MAGIC_STORAGE_TAG[1]
            || data[2] != MAGIC_STORAGE_TAG[2]
            || data[3] != MAGIC_STORAGE_TAG[3]
            || data[4] != MAGIC_STORAGE_VERSION
        {
            return None;
        }
        let flags = u16::from_le_bytes([data[5], data[6]]);
        if flags & !0x03FF != 0 {
            return None;
        }
        Some(Self {
            swap_control_capslock: flags & (1 << 0) != 0,
            capslock_to_control: flags & (1 << 1) != 0,
            swap_lalt_lgui: flags & (1 << 2) != 0,
            swap_ralt_rgui: flags & (1 << 3) != 0,
            no_gui: flags & (1 << 4) != 0,
            swap_grave_esc: flags & (1 << 5) != 0,
            swap_backslash_backspace: flags & (1 << 6) != 0,
            swap_lctl_lgui: flags & (1 << 7) != 0,
            swap_rctl_rgui: flags & (1 << 8) != 0,
            swap_escape_capslock: flags & (1 << 9) != 0,
        })
    }

    /// Apply one press-triggered QMK magic action.
    pub fn apply(&mut self, action: MagicAction) {
        match action {
            MagicAction::SwapControlCapsLock => self.swap_control_capslock = true,
            MagicAction::UnswapControlCapsLock => self.swap_control_capslock = false,
            MagicAction::ToggleControlCapsLock => self.swap_control_capslock = !self.swap_control_capslock,
            MagicAction::CapsLockAsControlOff => self.capslock_to_control = false,
            MagicAction::CapsLockAsControlOn => self.capslock_to_control = true,
            MagicAction::SwapLAltLGui => self.swap_lalt_lgui = true,
            MagicAction::UnswapLAltLGui => self.swap_lalt_lgui = false,
            MagicAction::SwapRAltRGui => self.swap_ralt_rgui = true,
            MagicAction::UnswapRAltRGui => self.swap_ralt_rgui = false,
            MagicAction::GuiOn => self.no_gui = false,
            MagicAction::GuiOff => self.no_gui = true,
            MagicAction::ToggleGui => self.no_gui = !self.no_gui,
            MagicAction::SwapGraveEscape => self.swap_grave_esc = true,
            MagicAction::UnswapGraveEscape => self.swap_grave_esc = false,
            MagicAction::SwapBackslashBackspace => self.swap_backslash_backspace = true,
            MagicAction::UnswapBackslashBackspace => self.swap_backslash_backspace = false,
            MagicAction::ToggleBackslashBackspace => self.swap_backslash_backspace = !self.swap_backslash_backspace,
            MagicAction::SwapAltGui => {
                self.swap_lalt_lgui = true;
                self.swap_ralt_rgui = true;
            }
            MagicAction::UnswapAltGui => {
                self.swap_lalt_lgui = false;
                self.swap_ralt_rgui = false;
            }
            MagicAction::ToggleAltGui => {
                let value = !self.swap_lalt_lgui;
                self.swap_lalt_lgui = value;
                self.swap_ralt_rgui = value;
            }
            MagicAction::SwapLCtrlLGui => self.swap_lctl_lgui = true,
            MagicAction::UnswapLCtrlLGui => self.swap_lctl_lgui = false,
            MagicAction::SwapRCtrlRGui => self.swap_rctl_rgui = true,
            MagicAction::UnswapRCtrlRGui => self.swap_rctl_rgui = false,
            MagicAction::SwapCtrlGui => {
                self.swap_lctl_lgui = true;
                self.swap_rctl_rgui = true;
            }
            MagicAction::UnswapCtrlGui => {
                self.swap_lctl_lgui = false;
                self.swap_rctl_rgui = false;
            }
            MagicAction::ToggleCtrlGui => {
                let value = !self.swap_lctl_lgui;
                self.swap_lctl_lgui = value;
                self.swap_rctl_rgui = value;
            }
            MagicAction::SwapEscapeCapsLock => self.swap_escape_capslock = true,
            MagicAction::UnswapEscapeCapsLock => self.swap_escape_capslock = false,
            MagicAction::ToggleEscapeCapsLock => self.swap_escape_capslock = !self.swap_escape_capslock,
        }
    }

    /// Apply QMK's `keycode_config()` precedence to one HID keycode.
    pub fn resolve_keycode(self, keycode: HidKeyCode) -> HidKeyCode {
        match keycode {
            HidKeyCode::CapsLock | HidKeyCode::LockingCapsLock => {
                if self.swap_control_capslock || self.capslock_to_control {
                    HidKeyCode::LCtrl
                } else if self.swap_escape_capslock {
                    HidKeyCode::Escape
                } else {
                    keycode
                }
            }
            HidKeyCode::LCtrl => {
                if self.swap_control_capslock {
                    HidKeyCode::CapsLock
                } else if self.swap_lctl_lgui {
                    if self.no_gui { HidKeyCode::No } else { HidKeyCode::LGui }
                } else {
                    HidKeyCode::LCtrl
                }
            }
            HidKeyCode::LAlt => {
                if self.swap_lalt_lgui {
                    if self.no_gui { HidKeyCode::No } else { HidKeyCode::LGui }
                } else {
                    HidKeyCode::LAlt
                }
            }
            HidKeyCode::LGui => {
                if self.swap_lalt_lgui {
                    HidKeyCode::LAlt
                } else if self.swap_lctl_lgui {
                    HidKeyCode::LCtrl
                } else if self.no_gui {
                    HidKeyCode::No
                } else {
                    HidKeyCode::LGui
                }
            }
            HidKeyCode::RCtrl => {
                if self.swap_rctl_rgui {
                    if self.no_gui { HidKeyCode::No } else { HidKeyCode::RGui }
                } else {
                    HidKeyCode::RCtrl
                }
            }
            HidKeyCode::RAlt => {
                if self.swap_ralt_rgui {
                    if self.no_gui { HidKeyCode::No } else { HidKeyCode::RGui }
                } else {
                    HidKeyCode::RAlt
                }
            }
            HidKeyCode::RGui => {
                if self.swap_ralt_rgui {
                    HidKeyCode::RAlt
                } else if self.swap_rctl_rgui {
                    HidKeyCode::RCtrl
                } else if self.no_gui {
                    HidKeyCode::No
                } else {
                    HidKeyCode::RGui
                }
            }
            HidKeyCode::Grave => {
                if self.swap_grave_esc {
                    HidKeyCode::Escape
                } else {
                    HidKeyCode::Grave
                }
            }
            HidKeyCode::Escape => {
                if self.swap_grave_esc {
                    HidKeyCode::Grave
                } else if self.swap_escape_capslock {
                    HidKeyCode::CapsLock
                } else {
                    HidKeyCode::Escape
                }
            }
            HidKeyCode::Backslash => {
                if self.swap_backslash_backspace {
                    HidKeyCode::Backspace
                } else {
                    HidKeyCode::Backslash
                }
            }
            HidKeyCode::Backspace => {
                if self.swap_backslash_backspace {
                    HidKeyCode::Backslash
                } else {
                    HidKeyCode::Backspace
                }
            }
            _ => keycode,
        }
    }

    /// Apply QMK's `mod_config()` swaps to a complete RMK modifier set.
    pub fn resolve_modifiers(self, modifiers: ModifierCombination) -> ModifierCombination {
        let mut bits = modifiers.into_bits();
        if self.swap_lalt_lgui {
            bits = swap_modifier_bits(bits, 0x04, 0x08);
        }
        if self.swap_ralt_rgui {
            bits = swap_modifier_bits(bits, 0x40, 0x80);
        }
        if self.swap_lctl_lgui {
            bits = swap_modifier_bits(bits, 0x01, 0x08);
        }
        if self.swap_rctl_rgui {
            bits = swap_modifier_bits(bits, 0x10, 0x80);
        }
        if self.no_gui {
            bits &= !(0x08 | 0x80);
        }
        ModifierCombination::from_bits(bits)
    }
}

fn swap_modifier_bits(bits: u8, first: u8, second: u8) -> u8 {
    let first_set = bits & first != 0;
    let second_set = bits & second != 0;
    let mut result = bits & !(first | second);
    if first_set {
        result |= second;
    }
    if second_set {
        result |= first;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_bitmap_advertises_only_the_thirty_supported_actions() {
        assert_eq!(supported_keycodes().count_ones(), 30);
        for offset in [0x11, 0x12, 0x13, 0x1E, 0x1F] {
            assert_eq!(supported_keycodes() & (1 << offset), 0);
        }
        assert_eq!(supported_keycode_bytes(), [0xFF, 0xFF, 0xF1, 0x3F, 0x07]);
    }

    #[test]
    fn supported_actions_round_trip_through_qmk_and_user_ids() {
        for offset in 0..=MAGIC_OFFSET_END {
            let keycode = KEYCODE_START + u16::from(offset);
            match MagicAction::from_keycode(keycode) {
                Some(action) => {
                    assert_eq!(action.keycode(), keycode);
                    assert_eq!(MagicAction::from_user_id(action.user_id()), Some(action));
                }
                None => assert!(MagicAction::from_user_id(USER_ID_START + offset).is_none()),
            }
        }
    }

    #[test]
    fn qmk_keycode_precedence_matches_magic_config() {
        let mut config = MagicConfig::default();
        config.swap_control_capslock = true;
        config.swap_escape_capslock = true;
        assert_eq!(config.resolve_keycode(HidKeyCode::CapsLock), HidKeyCode::LCtrl);
        assert_eq!(config.resolve_keycode(HidKeyCode::Escape), HidKeyCode::CapsLock);

        config.swap_grave_esc = true;
        assert_eq!(config.resolve_keycode(HidKeyCode::Escape), HidKeyCode::Grave);
    }

    #[test]
    fn modifier_swaps_follow_qmk_order_and_gui_disable() {
        let config = MagicConfig {
            swap_lalt_lgui: true,
            swap_lctl_lgui: true,
            no_gui: true,
            ..Default::default()
        };
        assert_eq!(
            config.resolve_modifiers(ModifierCombination::LALT),
            ModifierCombination::LCTRL
        );
        assert_eq!(
            config.resolve_modifiers(ModifierCombination::LGUI),
            ModifierCombination::LALT
        );
    }

    #[test]
    fn storage_payload_is_tagged_versioned_and_round_trips() {
        let config = MagicConfig {
            swap_control_capslock: true,
            capslock_to_control: true,
            swap_lalt_lgui: true,
            swap_ralt_rgui: true,
            no_gui: true,
            swap_grave_esc: true,
            swap_backslash_backspace: true,
            swap_lctl_lgui: true,
            swap_rctl_rgui: true,
            swap_escape_capslock: true,
        };
        let payload = config.to_storage_payload();
        assert_eq!(&payload[..4], b"RMKM");
        assert_eq!(payload[4], 1);
        assert_eq!(MagicConfig::from_storage_payload(&payload), Some(config));

        let mut conflict = payload;
        conflict[0] = 0;
        assert_eq!(MagicConfig::from_storage_payload(&conflict), None);
        let mut future = payload;
        future[6] |= 0x04;
        assert_eq!(MagicConfig::from_storage_payload(&future), None);
    }
}
