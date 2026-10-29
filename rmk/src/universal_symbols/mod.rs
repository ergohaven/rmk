//! Firmware-native punctuation that follows the active EN/RU host layout.
//!
//! IDs live in the upper half of [`Action::User`](rmk_types::action::Action::User)
//! so they cannot collide with Vial's 64 keyboard-specific user keycodes. New
//! layouts belong in their own file under `layout/`.

mod layout;

use core::sync::atomic::{AtomicBool, Ordering};

// A boot hand-off, not a second persisted platform setting. Each Keyboard owns
// its two-way platform state, restored before it starts processing events.
static BOOT_MAC: AtomicBool = AtomicBool::new(false);
// Geometry has one live snapshot, sourced exclusively from Vial LayoutConfig.
static ISO_GEOMETRY: AtomicBool = AtomicBool::new(false);

pub(crate) fn restore_platform(mac: bool) {
    BOOT_MAC.store(mac, Ordering::Relaxed);
}

pub(crate) fn set_layout_options(options: u32) {
    // Boards explicitly select the bit matching their Vial option packing.
    // The original K:04 contract remains bit 2; new options are prepended so
    // existing stored option bits retain their meanings.
    let iso_mask = if cfg!(feature = "universal_symbols_iso_bit0") {
        1
    } else if cfg!(feature = "universal_symbols_iso_bit1") {
        2
    } else {
        4
    };
    ISO_GEOMETRY.store(
        cfg!(feature = "universal_symbols_iso") && options & iso_mask != 0,
        Ordering::Relaxed,
    );
}

pub const USER_TOGGLE: u8 = 0x80;
pub const USER_SYNC: u8 = 0x81;
pub const USER_SET_ENGLISH: u8 = 0x82;
pub const USER_SET_RUSSIAN: u8 = 0x83;
pub const USER_TOGGLE_MACOS: u8 = 0x84;
pub const USER_SYMBOL_START: u8 = 0x90;
pub const USER_SYMBOL_END: u8 = 0xB0;
pub const USER_RUSSIAN_LETTER_START: u8 = 0xB0;
pub const USER_RUSSIAN_LETTER_END: u8 = 0xB4;
pub const USER_NUMERO: u8 = 0xB4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum HostLayout {
    #[default]
    English,
    Russian,
}

impl HostLayout {
    const fn toggled(self) -> Self {
        match self {
            Self::English => Self::Russian,
            Self::Russian => Self::English,
        }
    }

    const fn from_host_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::English),
            1 => Some(Self::Russian),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Platform {
    #[default]
    Pc,
    Mac,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Symbol {
    Dot,
    Comma,
    Semicolon,
    Colon,
    Exclamation,
    Question,
    Slash,
    Grave,
    Tilde,
    Apostrophe,
    Quote,
    LeftParenthesis,
    RightParenthesis,
    LeftBracket,
    RightBracket,
    LeftBrace,
    RightBrace,
    LessThan,
    GreaterThan,
    Minus,
    Plus,
    Asterisk,
    Equal,
    Hash,
    At,
    Dollar,
    Percent,
    Caret,
    Ampersand,
    Pipe,
    Backslash,
    Underscore,
}

impl Symbol {
    pub(crate) const fn from_user_id(id: u8) -> Option<Self> {
        if id < USER_SYMBOL_START || id >= USER_SYMBOL_END {
            return None;
        }
        Some(match id - USER_SYMBOL_START {
            0 => Self::Dot,
            1 => Self::Comma,
            2 => Self::Semicolon,
            3 => Self::Colon,
            4 => Self::Exclamation,
            5 => Self::Question,
            6 => Self::Slash,
            7 => Self::Grave,
            8 => Self::Tilde,
            9 => Self::Apostrophe,
            10 => Self::Quote,
            11 => Self::LeftParenthesis,
            12 => Self::RightParenthesis,
            13 => Self::LeftBracket,
            14 => Self::RightBracket,
            15 => Self::LeftBrace,
            16 => Self::RightBrace,
            17 => Self::LessThan,
            18 => Self::GreaterThan,
            19 => Self::Minus,
            20 => Self::Plus,
            21 => Self::Asterisk,
            22 => Self::Equal,
            23 => Self::Hash,
            24 => Self::At,
            25 => Self::Dollar,
            26 => Self::Percent,
            27 => Self::Caret,
            28 => Self::Ampersand,
            29 => Self::Pipe,
            30 => Self::Backslash,
            31 => Self::Underscore,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RussianLetter {
    Kha,
    Be,
    Yu,
    HardSign,
}

impl RussianLetter {
    pub(crate) const fn from_user_id(id: u8) -> Option<Self> {
        if id < USER_RUSSIAN_LETTER_START || id >= USER_RUSSIAN_LETTER_END {
            return None;
        }
        Some(match id - USER_RUSSIAN_LETTER_START {
            0 => Self::Kha,
            1 => Self::Be,
            2 => Self::Yu,
            3 => Self::HardSign,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    None,
    SwitchLayout,
    Type(layout::ResolvedStroke),
    TypeRussianLetter(rmk_types::keycode::HidKeyCode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct State {
    layout: HostLayout,
    platform: Platform,
    last_host_layout: Option<HostLayout>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            layout: HostLayout::English,
            platform: if BOOT_MAC.load(Ordering::Relaxed) {
                Platform::Mac
            } else {
                Platform::Pc
            },
            last_host_layout: None,
        }
    }
}

impl State {
    pub(crate) fn handle(&mut self, user_id: u8, host_layout: Option<u8>) -> Option<Command> {
        self.sync_from_host(host_layout);

        let command = match user_id {
            USER_TOGGLE => {
                self.layout = self.layout.toggled();
                Command::SwitchLayout
            }
            USER_SYNC => {
                self.layout = self.layout.toggled();
                Command::None
            }
            USER_SET_ENGLISH => self.set_layout(HostLayout::English),
            USER_SET_RUSSIAN => self.set_layout(HostLayout::Russian),
            USER_TOGGLE_MACOS => {
                self.platform = match self.platform {
                    Platform::Pc => Platform::Mac,
                    Platform::Mac => Platform::Pc,
                };
                Command::None
            }
            USER_NUMERO => Command::Type(layout::resolve_numero(self.layout)),
            id => {
                if let Some(letter) = RussianLetter::from_user_id(id) {
                    match layout::resolve_russian_letter(self.layout, letter) {
                        Some(keycode) => Command::TypeRussianLetter(keycode),
                        None => Command::None,
                    }
                } else {
                    Command::Type(layout::resolve(self.layout, self.platform, Symbol::from_user_id(id)?))
                }
            }
        };
        Some(command)
    }

    pub(crate) const fn platform(&self) -> Platform {
        self.platform
    }

    fn set_layout(&mut self, layout: HostLayout) -> Command {
        if self.layout == layout {
            Command::None
        } else {
            self.layout = layout;
            Command::SwitchLayout
        }
    }

    fn sync_from_host(&mut self, code: Option<u8>) {
        let Some(layout) = code.and_then(HostLayout::from_host_code) else {
            return;
        };
        if self.last_host_layout != Some(layout) {
            self.layout = layout;
            self.last_host_layout = Some(layout);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    // Mirror the production board feature selection without changing runtime code.
    pub(crate) const ISO_LAYOUT_OPTION_MASK: u32 = if cfg!(feature = "universal_symbols_iso_bit0") {
        1
    } else if cfg!(feature = "universal_symbols_iso_bit1") {
        2
    } else {
        4
    };

    use rmk_types::keycode::HidKeyCode;
    use rmk_types::modifier::ModifierCombination;

    use super::*;

    #[test]
    fn stable_symbol_id_range_covers_all_punctuation() {
        for id in USER_SYMBOL_START..USER_SYMBOL_END {
            assert!(Symbol::from_user_id(id).is_some(), "missing user id {id:#04x}");
        }
        assert!(Symbol::from_user_id(USER_SYMBOL_START - 1).is_none());
        assert!(Symbol::from_user_id(USER_SYMBOL_END).is_none());
    }

    #[test]
    fn stable_russian_letter_ids_map_only_in_russian_layout() {
        let expected = [
            HidKeyCode::LeftBracket,
            HidKeyCode::Comma,
            HidKeyCode::Dot,
            HidKeyCode::RightBracket,
        ];
        let mut state = State::default();

        for id in USER_RUSSIAN_LETTER_START..USER_RUSSIAN_LETTER_END {
            assert_eq!(state.handle(id, None), Some(Command::None));
        }
        assert!(RussianLetter::from_user_id(USER_RUSSIAN_LETTER_START - 1).is_none());
        assert!(RussianLetter::from_user_id(USER_RUSSIAN_LETTER_END).is_none());

        assert_eq!(state.handle(USER_SYNC, None), Some(Command::None));
        for (id, keycode) in (USER_RUSSIAN_LETTER_START..USER_RUSSIAN_LETTER_END).zip(expected) {
            assert_eq!(state.handle(id, None), Some(Command::TypeRussianLetter(keycode)));
        }
    }

    #[test]
    fn russian_pc_uses_native_period_and_temporarily_switches_for_brace() {
        let mut state = State::default();
        assert_eq!(state.handle(USER_SYNC, None), Some(Command::None));
        let Command::Type(dot) = state.handle(USER_SYMBOL_START, None).unwrap() else {
            panic!("expected dot stroke");
        };
        assert_eq!(dot.stroke.keycode, HidKeyCode::Slash);
        assert_eq!(dot.stroke.modifiers, ModifierCombination::new());
        assert!(!dot.temporary_layout_switch);

        let Command::Type(brace) = state.handle(USER_SYMBOL_START + 15, None).unwrap() else {
            panic!("expected brace stroke");
        };
        assert_eq!(brace.stroke.keycode, HidKeyCode::LeftBracket);
        assert_eq!(brace.stroke.modifiers, ModifierCombination::LSHIFT);
        assert!(brace.temporary_layout_switch);
    }

    #[test]
    fn host_layout_sync_only_applies_when_host_value_changes() {
        let mut state = State::default();
        assert_eq!(
            state.handle(USER_SYMBOL_START, Some(0)).unwrap(),
            Command::Type(layout::resolve(HostLayout::English, Platform::Pc, Symbol::Dot))
        );
        assert_eq!(state.handle(USER_SYNC, Some(0)), Some(Command::None));

        let Command::Type(dot) = state.handle(USER_SYMBOL_START, Some(0)).unwrap() else {
            panic!("expected dot stroke");
        };
        assert_eq!(dot.stroke.keycode, HidKeyCode::Slash);

        let Command::Type(dot) = state.handle(USER_SYMBOL_START, Some(1)).unwrap() else {
            panic!("expected dot stroke");
        };
        assert_eq!(dot.stroke.keycode, HidKeyCode::Slash);
        assert!(!dot.temporary_layout_switch);
    }

    #[test]
    fn mac_russian_asterisk_uses_english_fallback_but_pc_stays_direct() {
        for platform in [Platform::Pc, Platform::Mac] {
            let result = layout::resolve(HostLayout::Russian, platform, Symbol::Asterisk);
            assert_eq!(result.stroke.keycode, HidKeyCode::Kc8);
            assert_eq!(result.stroke.modifiers, ModifierCombination::LSHIFT);
            assert_eq!(result.temporary_layout_switch, platform == Platform::Mac);
        }
    }

    #[cfg(feature = "universal_symbols_iso")]
    #[test]
    fn iso_geometry_only_remaps_mac_universal_grave_tilde_including_ru_fallback() {
        set_layout_options(ISO_LAYOUT_OPTION_MASK);
        for host in [HostLayout::English, HostLayout::Russian] {
            for platform in [Platform::Pc, Platform::Mac] {
                for symbol in [Symbol::Grave, Symbol::Tilde] {
                    let result = layout::resolve(host, platform, symbol);
                    assert_eq!(
                        result.stroke.keycode,
                        if platform == Platform::Mac {
                            HidKeyCode::NonusBackslash
                        } else {
                            HidKeyCode::Grave
                        }
                    );
                    assert_eq!(
                        result.stroke.modifiers,
                        if symbol == Symbol::Tilde {
                            ModifierCombination::LSHIFT
                        } else {
                            ModifierCombination::new()
                        }
                    );
                    assert_eq!(result.temporary_layout_switch, host == HostLayout::Russian);
                }
            }
        }
    }

    #[cfg(feature = "universal_symbols_iso")]
    #[test]
    fn geometry_write_preserves_legacy_bits_and_ansi_strokes() {
        for legacy_bits in (0..8).filter(|options| options & ISO_LAYOUT_OPTION_MASK == 0) {
            set_layout_options(legacy_bits);
            for platform in [Platform::Pc, Platform::Mac] {
                assert_eq!(
                    layout::resolve(HostLayout::English, platform, Symbol::Grave)
                        .stroke
                        .keycode,
                    HidKeyCode::Grave
                );
            }
            set_layout_options(legacy_bits | ISO_LAYOUT_OPTION_MASK);
            assert_eq!(
                layout::resolve(HostLayout::English, Platform::Mac, Symbol::Grave)
                    .stroke
                    .keycode,
                HidKeyCode::NonusBackslash
            );
            set_layout_options(legacy_bits);
            assert_eq!(
                layout::resolve(HostLayout::English, Platform::Mac, Symbol::Grave)
                    .stroke
                    .keycode,
                HidKeyCode::Grave
            );
        }
    }

    #[test]
    fn restored_platform_keeps_exact_two_way_toggle_semantics() {
        restore_platform(true);
        let mut s = State::default();
        assert_eq!(s.platform(), Platform::Mac);
        assert_eq!(s.handle(USER_TOGGLE_MACOS, None), Some(Command::None));
        assert_eq!(s.platform(), Platform::Pc);
        s.handle(USER_TOGGLE_MACOS, None);
        assert_eq!(s.platform(), Platform::Mac);
        restore_platform(false);
        assert_eq!(State::default().platform(), Platform::Pc);
    }

    #[test]
    fn mac_mode_uses_mac_russian_punctuation() {
        let mut state = State::default();
        state.handle(USER_SYNC, None);
        state.handle(USER_TOGGLE_MACOS, None);
        let Command::Type(dot) = state.handle(USER_SYMBOL_START, None).unwrap() else {
            panic!("expected dot stroke");
        };
        assert_eq!(dot.stroke.keycode, HidKeyCode::Kc7);
        assert_eq!(dot.stroke.modifiers, ModifierCombination::LSHIFT);
    }
}
