use embassy_time::{Duration, Instant};
use rmk_types::action::Action;
use rmk_types::keycode::HidKeyCode;
use rmk_types::modifier::ModifierCombination;

use super::Keyboard;
use crate::event::{KeyboardEvent, KeyboardEventSource};
use crate::universal_symbols::{Command, HostLayout};

#[derive(Default)]
pub(super) struct LanguageHelpers {
    english_word: bool,
    mod_press: Option<(KeyboardEventSource, Instant)>,
    stored_layout: HostLayout,
    storing: bool,
    sync_after: Option<Instant>,
    host_layout_revision: Option<u32>,
}

impl Keyboard<'_> {
    pub(super) fn sync_ruen_host_layout(&mut self) {
        if self.ruen.storing || self.ruen.sync_after.is_some_and(|deadline| Instant::now() < deadline) {
            return;
        }
        let (layout, revision) = crate::host_data::layout_snapshot();
        if self.ruen.host_layout_revision != Some(revision) {
            if let Some(layout) = layout.and_then(HostLayout::from_host_code) {
                self.universal_symbols.set_layout(layout);
            }
            self.ruen.host_layout_revision = Some(revision);
        }
    }

    async fn set_ruen_layout(&mut self, layout: HostLayout) -> bool {
        let previous = self.universal_symbols;
        if self.universal_symbols.set_layout(layout) == Command::None {
            return false;
        }
        let sent = self
            .send_universal_symbols_layout_switch(self.universal_symbols.platform())
            .await;
        if !sent {
            self.universal_symbols = previous;
        }
        sent
    }

    pub(super) async fn process_ruen_user_action(&mut self, id: u8, event: KeyboardEvent) -> Option<bool> {
        if !(56..=59).contains(&id) {
            return None;
        }
        if !event.pressed && id != 57 {
            return Some(false);
        }
        self.sync_ruen_host_layout();
        let sent = match id {
            56 if self.universal_symbols.layout() == HostLayout::Russian && !self.ruen.english_word => {
                let shift_mask = ModifierCombination::LSHIFT | ModifierCombination::RSHIFT;
                let shift = (self.resolve_modifiers(true) & shift_mask).into_bits() != 0;
                let saved_osm = if self
                    .osm_state
                    .value()
                    .is_some_and(|mods| (*mods & shift_mask).into_bits() != 0)
                {
                    Some((core::mem::take(&mut self.osm_state), self.osm_deadline.take()))
                } else {
                    None
                };
                let sent = self.set_ruen_layout(HostLayout::English).await;
                if self.universal_symbols.layout() == HostLayout::English {
                    self.ruen.english_word = true;
                    if shift {
                        self.caps_word.activate();
                    }
                } else if let Some((state, deadline)) = saved_osm {
                    self.osm_state = state;
                    self.osm_deadline = deadline;
                }
                sent
            }
            57 if event.pressed => {
                let target = self.universal_symbols.layout().toggled();
                let sent = self.set_ruen_layout(target).await;
                self.ruen.mod_press =
                    (self.universal_symbols.layout() == target).then_some((event.source, Instant::now()));
                sent
            }
            57 => {
                if let Some((source, pressed_at)) = self.ruen.mod_press
                    && source == event.source
                {
                    self.ruen.mod_press = None;
                    if pressed_at.elapsed()
                        >= Self::morse_timeout(
                            self.keymap,
                            &crate::types::action::KeyAction::Single(Action::User(id)),
                            true,
                        )
                    {
                        let target = self.universal_symbols.layout().toggled();
                        self.set_ruen_layout(target).await
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            58 => {
                self.ruen.stored_layout = self.universal_symbols.layout();
                self.ruen.storing = true;
                false
            }
            59 => {
                let sent = self.set_ruen_layout(self.ruen.stored_layout).await;
                self.ruen.storing = false;
                self.ruen.sync_after = Some(Instant::now() + Duration::from_millis(500));
                sent
            }
            _ => false,
        };
        Some(sent)
    }

    pub(super) async fn finish_ruen_word(&mut self, key: HidKeyCode, pressed: bool) {
        if pressed
            && self.ruen.english_word
            && matches!(
                key,
                HidKeyCode::Space | HidKeyCode::Enter | HidKeyCode::Escape | HidKeyCode::Minus
            )
        {
            self.sync_ruen_host_layout();
            self.set_ruen_layout(HostLayout::Russian).await;
            if self.universal_symbols.layout() == HostLayout::Russian {
                self.ruen.english_word = false;
                self.caps_word.deactivate();
            }
        }
    }
}
