use rmk_types::keycode::HidKeyCode;

use super::{ResolvedStroke, Stroke, en};
use crate::universal_symbols::{Platform, RussianLetter, Symbol};

pub(super) const fn letter_keycode(letter: RussianLetter) -> HidKeyCode {
    match letter {
        RussianLetter::Kha => HidKeyCode::LeftBracket,
        RussianLetter::Be => HidKeyCode::Comma,
        RussianLetter::Yu => HidKeyCode::Dot,
        RussianLetter::HardSign => HidKeyCode::RightBracket,
    }
}

pub(super) const fn stroke(platform: Platform, symbol: Symbol) -> ResolvedStroke {
    let direct = match (platform, symbol) {
        (Platform::Pc, Symbol::Dot) => Some(Stroke::plain(HidKeyCode::Slash)),
        (Platform::Mac, Symbol::Dot) => Some(Stroke::shifted(HidKeyCode::Kc7)),
        (Platform::Pc, Symbol::Comma) => Some(Stroke::shifted(HidKeyCode::Slash)),
        (Platform::Mac, Symbol::Comma) => Some(Stroke::shifted(HidKeyCode::Kc6)),
        (Platform::Pc, Symbol::Semicolon) => Some(Stroke::shifted(HidKeyCode::Kc4)),
        (Platform::Mac, Symbol::Semicolon) => Some(Stroke::shifted(HidKeyCode::Kc8)),
        (Platform::Pc, Symbol::Colon) => Some(Stroke::shifted(HidKeyCode::Kc6)),
        (Platform::Mac, Symbol::Colon) => Some(Stroke::shifted(HidKeyCode::Kc5)),
        (_, Symbol::Exclamation) => Some(Stroke::shifted(HidKeyCode::Kc1)),
        (Platform::Pc, Symbol::Question) => Some(Stroke::shifted(HidKeyCode::Kc7)),
        (Platform::Mac, Symbol::Question) => Some(Stroke::shifted(HidKeyCode::Slash)),
        (Platform::Pc, Symbol::Slash) => Some(Stroke::shifted(HidKeyCode::Backslash)),
        (Platform::Mac, Symbol::Slash) => Some(Stroke::plain(HidKeyCode::Slash)),
        (_, Symbol::Quote) => Some(Stroke::shifted(HidKeyCode::Kc2)),
        (_, Symbol::LeftParenthesis) => Some(Stroke::shifted(HidKeyCode::Kc9)),
        (_, Symbol::RightParenthesis) => Some(Stroke::shifted(HidKeyCode::Kc0)),
        (_, Symbol::Minus) => Some(Stroke::plain(HidKeyCode::Minus)),
        (_, Symbol::Plus) => Some(Stroke::shifted(HidKeyCode::Equal)),
        (Platform::Pc, Symbol::Asterisk) => Some(Stroke::shifted(HidKeyCode::Kc8)),
        (_, Symbol::Equal) => Some(Stroke::plain(HidKeyCode::Equal)),
        (Platform::Pc, Symbol::Percent) => Some(Stroke::shifted(HidKeyCode::Kc5)),
        (Platform::Mac, Symbol::Percent) => Some(Stroke::shifted(HidKeyCode::Kc4)),
        (_, Symbol::Underscore) => Some(Stroke::shifted(HidKeyCode::Minus)),
        _ => None,
    };

    match direct {
        Some(stroke) => ResolvedStroke::current(stroke),
        None => ResolvedStroke::english(en::stroke(symbol)),
    }
}
