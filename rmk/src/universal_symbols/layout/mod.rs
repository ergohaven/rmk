mod en;
mod ru;

use rmk_types::keycode::HidKeyCode;
use rmk_types::modifier::ModifierCombination;

use super::{HostLayout, Platform, RussianLetter, Symbol};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stroke {
    pub(crate) keycode: HidKeyCode,
    pub(crate) modifiers: ModifierCombination,
}

impl Stroke {
    pub(crate) const fn plain(keycode: HidKeyCode) -> Self {
        Self {
            keycode,
            modifiers: ModifierCombination::new(),
        }
    }

    pub(crate) const fn shifted(keycode: HidKeyCode) -> Self {
        Self {
            keycode,
            modifiers: ModifierCombination::LSHIFT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedStroke {
    pub(crate) stroke: Stroke,
    pub(crate) temporary_layout_switch: bool,
}

impl ResolvedStroke {
    const fn current(stroke: Stroke) -> Self {
        Self {
            stroke,
            temporary_layout_switch: false,
        }
    }

    const fn english(stroke: Stroke) -> Self {
        Self {
            stroke,
            temporary_layout_switch: true,
        }
    }
}

// Unlike the legacy punctuation, № is native to Russian on both PC and Mac.
// A temporary switch is symmetric: toggle before typing and toggle back after,
// without changing State's source language or the host-sync snapshot.
pub(crate) const fn resolve_numero(layout: HostLayout) -> ResolvedStroke {
    ResolvedStroke {
        stroke: Stroke::shifted(HidKeyCode::Kc3),
        temporary_layout_switch: matches!(layout, HostLayout::English),
    }
}

pub(crate) fn resolve(layout: HostLayout, platform: Platform, symbol: Symbol) -> ResolvedStroke {
    let mut resolved = match layout {
        HostLayout::English => ResolvedStroke::current(en::stroke(symbol)),
        HostLayout::Russian => ru::stroke(platform, symbol),
    };
    // Apply after the Russian English-fallback decision so it uses the same
    // physical geometry. Ordinary Grave never enters this symbol resolver.
    if platform == Platform::Mac
        && super::ISO_GEOMETRY.load(super::Ordering::Relaxed)
        && matches!(symbol, Symbol::Grave | Symbol::Tilde)
    {
        resolved.stroke.keycode = HidKeyCode::NonusBackslash;
    }
    resolved
}

pub(crate) const fn resolve_russian_letter(layout: HostLayout, letter: RussianLetter) -> Option<HidKeyCode> {
    match layout {
        HostLayout::English => None,
        HostLayout::Russian => Some(ru::letter_keycode(letter)),
    }
}
