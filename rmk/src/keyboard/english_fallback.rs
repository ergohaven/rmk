use embassy_time::{Duration, Instant};
use rmk_types::keycode::HidKeyCode;

use super::Keyboard;
use crate::universal_symbols::{HostLayout, Symbol};

impl Keyboard<'_> {
    pub(super) async fn expire_english_fallback(&mut self) {
        if self.english_fallback.is_some_and(|deadline| deadline <= Instant::now()) {
            self.restore_english_fallback().await;
        }
    }

    pub(super) async fn restore_english_fallback(&mut self) {
        if self.english_fallback.is_none() {
            return;
        }
        if self.universal_symbols.layout() != HostLayout::Russian
            && !self
                .send_universal_symbols_layout_switch(self.universal_symbols.platform())
                .await
        {
            // A full 6KRO report can temporarily prevent the Space needed for switching.
            self.english_fallback = Some(Instant::now() + Duration::from_millis(10));
            return;
        }
        #[cfg(feature = "ergohaven_language_helpers")]
        self.sync_ruen_host_layout();
        #[cfg(not(feature = "ergohaven_language_helpers"))]
        self.universal_symbols
            .observe_host_layout(crate::host_data::snapshot().layout);
        self.universal_symbols.set_layout(HostLayout::Russian);
        self.english_fallback = None;
    }

    pub(super) async fn finish_english_fallback_for_key(&mut self, key: HidKeyCode, pressed: bool) {
        let code = key as u8;
        if pressed
            && self.english_fallback.is_some()
            && [
                (HidKeyCode::A as u8..=HidKeyCode::Z as u8),
                (HidKeyCode::LeftBracket as u8..=HidKeyCode::RightBracket as u8),
                (HidKeyCode::Semicolon as u8..=HidKeyCode::Slash as u8),
            ]
            .iter()
            .any(|range| range.contains(&code))
        {
            self.restore_english_fallback().await;
        }
    }

    pub(super) fn extend_english_fallback(&mut self, id: u8) {
        if self.english_fallback.is_some()
            && matches!(
                Symbol::from_user_id(id),
                Some(
                    Symbol::Grave
                        | Symbol::Tilde
                        | Symbol::Apostrophe
                        | Symbol::LeftBracket
                        | Symbol::RightBracket
                        | Symbol::LeftBrace
                        | Symbol::RightBrace
                        | Symbol::LessThan
                        | Symbol::GreaterThan
                        | Symbol::At
                        | Symbol::Hash
                        | Symbol::Dollar
                        | Symbol::Caret
                        | Symbol::Ampersand
                        | Symbol::Pipe
                        | Symbol::Asterisk
                )
            )
        {
            self.english_fallback = Some(Instant::now() + Duration::from_millis(500));
        }
    }
}
