//! Modifier order at the keyboard task's report boundary.
use rmk::k;
use rmk::test_support::test_block_on;
use rmk::types::action::{Action, KeyAction};
use rmk::types::keycode::HidKeyCode;
use rmk::types::modifier::ModifierCombination;

use crate::simulator::SimKeyboard;

#[test]
fn modified_shortcut_orders_modifiers_before_key_and_releases_key_first() {
    test_block_on(async {
        let mods = ModifierCombination::LCTRL | ModifierCombination::LGUI;
        let shortcut = KeyAction::Single(Action::KeyWithModifier(HidKeyCode::T, mods));
        let mut keyboard = SimKeyboard::builder([[[shortcut]]]).build().await;
        keyboard
            .tap(0, 0, 10)
            .expect_keys_with_mods(mods.into_bits(), [])
            .expect_keys_with_mods(mods.into_bits(), [HidKeyCode::T])
            .expect_keys_with_mods(mods.into_bits(), [])
            .expect_keys([])
            .run()
            .await;
    });
}

#[test]
fn modified_shortcut_preserves_other_held_keys_and_modifiers() {
    test_block_on(async {
        let mods = ModifierCombination::LCTRL | ModifierCombination::LGUI;
        for held in [
            ModifierCombination::new(),
            ModifierCombination::LCTRL,
            ModifierCombination::LSHIFT,
            mods,
        ] {
            let shortcut = KeyAction::Single(Action::KeyWithModifier(HidKeyCode::T, mods));
            let mut keyboard = SimKeyboard::builder([[[k!(A), KeyAction::Single(Action::Modifier(held)), shortcut]]])
                .build()
                .await;
            if held.into_bits() != 0 {
                keyboard.press(0, 1).expect_keys_with_mods(held.into_bits(), []);
            }
            keyboard
                .press(0, 0)
                .expect_keys_with_mods(held.into_bits(), [HidKeyCode::A]);
            keyboard.press(0, 2);
            if (held | mods) != held {
                keyboard.expect_keys_with_mods((held | mods).into_bits(), [HidKeyCode::A]);
            }
            keyboard.expect_keys_with_mods((held | mods).into_bits(), [HidKeyCode::A, HidKeyCode::T]);
            keyboard
                .release(0, 2)
                .expect_keys_with_mods((held | mods).into_bits(), [HidKeyCode::A]);
            if (held | mods) != held {
                keyboard.expect_keys_with_mods(held.into_bits(), [HidKeyCode::A]);
            }
            keyboard.release(0, 0).expect_keys_with_mods(held.into_bits(), []);
            if held.into_bits() != 0 {
                keyboard.release(0, 1).expect_keys([]);
            }
            keyboard.run().await;
        }
    });
}

#[test]
fn modified_shortcut_caps_word_pre_report_uses_the_current_key() {
    test_block_on(async {
        use rmk::types::action::KeyboardAction;
        for (key, shifted) in [
            (HidKeyCode::T, true),
            (HidKeyCode::Kc1, false),
            (HidKeyCode::Escape, false),
        ] {
            let shortcut = KeyAction::Single(Action::KeyWithModifier(key, ModifierCombination::LCTRL));
            let caps = KeyAction::Single(Action::KeyboardControl(KeyboardAction::CapsWordToggle));
            let mut keyboard = SimKeyboard::builder([[[caps, shortcut]]]).build().await;
            let mods = if shifted {
                ModifierCombination::LCTRL | ModifierCombination::LSHIFT
            } else {
                ModifierCombination::LCTRL
            };
            keyboard
                .tap(0, 0, 10)
                .tap(0, 1, 10)
                .expect_keys_with_mods(mods.into_bits(), [])
                .expect_keys_with_mods(mods.into_bits(), [key])
                .expect_keys_with_mods(ModifierCombination::LCTRL.into_bits(), [])
                .expect_keys([])
                .run()
                .await;
        }
    });
}

#[cfg(feature = "universal_symbols")]
#[test]
fn one_key_ctrl_space_paces_the_layout_shortcut_even_on_a_quick_tap() {
    test_block_on(async {
        let ctrl = ModifierCombination::LCTRL;
        let shortcut = KeyAction::Single(Action::KeyWithModifier(HidKeyCode::Space, ctrl));
        let mut keyboard = SimKeyboard::builder([[[k!(A), shortcut]]]).build().await;
        keyboard
            .press(0, 0)
            .expect_keys([HidKeyCode::A])
            .press(0, 1)
            .expect_keys_with_mods(ctrl.into_bits(), [HidKeyCode::A])
            .release(0, 1)
            .expect_no_report(19)
            .expect_keys_with_mods(ctrl.into_bits(), [HidKeyCode::A, HidKeyCode::Space])
            .expect_no_report(19)
            .expect_keys([HidKeyCode::A, HidKeyCode::Space])
            .expect_no_report(9)
            .expect_keys([HidKeyCode::A])
            .release(0, 0)
            .expect_keys([])
            .run()
            .await;
    });
}

#[cfg(feature = "universal_symbols")]
#[test]
fn one_key_ctrl_space_keeps_physical_ctrl_down() {
    test_block_on(async {
        let ctrl = ModifierCombination::LCTRL;
        let shortcut = KeyAction::Single(Action::KeyWithModifier(HidKeyCode::Space, ctrl));
        let mut keyboard = SimKeyboard::builder([[[k!(LCtrl), shortcut]]]).build().await;
        keyboard
            .press(0, 0)
            .expect_keys_with_mods(ctrl.into_bits(), [])
            .press(0, 1)
            .expect_keys_with_mods(ctrl.into_bits(), [HidKeyCode::Space])
            .release(0, 1)
            .expect_keys_with_mods(ctrl.into_bits(), [])
            .release(0, 0)
            .expect_keys([])
            .run()
            .await;
    });
}

#[cfg(feature = "universal_symbols")]
#[test]
fn one_key_ctrl_space_preserves_release_order_when_ctrl_or_space_was_already_held() {
    test_block_on(async {
        let ctrl = ModifierCombination::LCTRL;
        let shortcut = KeyAction::Single(Action::KeyWithModifier(HidKeyCode::Space, ctrl));
        for (physical, is_ctrl) in [(k!(LCtrl), true), (k!(Space), false)] {
            let mut keyboard = SimKeyboard::builder([[[physical, shortcut]]]).build().await;
            keyboard.press(0, 0);
            if is_ctrl {
                keyboard.expect_keys_with_mods(ctrl.into_bits(), []);
            } else {
                keyboard.expect_keys([HidKeyCode::Space]);
            }

            keyboard.press(0, 1);
            if !is_ctrl {
                keyboard.expect_keys_with_mods(ctrl.into_bits(), [HidKeyCode::Space]);
            }
            keyboard
                .expect_keys_with_mods(ctrl.into_bits(), [HidKeyCode::Space])
                .release(0, 0)
                .expect_keys_with_mods(ctrl.into_bits(), [HidKeyCode::Space])
                .release(0, 1)
                .expect_keys_with_mods(ctrl.into_bits(), [])
                .expect_keys([])
                .run()
                .await;
        }
    });
}

#[cfg(feature = "universal_symbols")]
#[test]
fn one_key_ctrl_space_preserves_release_order_with_one_shot_modifiers() {
    test_block_on(async {
        let ctrl = ModifierCombination::LCTRL;
        let shortcut = KeyAction::Single(Action::KeyWithModifier(HidKeyCode::Space, ctrl));
        for modifier in [ctrl, ModifierCombination::LSHIFT] {
            let one_shot = KeyAction::Single(Action::OneShotModifier(modifier));
            let mut keyboard = SimKeyboard::builder([[[one_shot, shortcut]]]).build().await;
            let combined = (ctrl | modifier).into_bits();
            keyboard.tap(0, 0, 10).tap(0, 1, 10);
            if modifier != ctrl {
                keyboard.expect_keys_with_mods(combined, []);
            }
            keyboard
                .expect_keys_with_mods(combined, [HidKeyCode::Space])
                .expect_keys_with_mods(ctrl.into_bits(), [])
                .expect_keys([])
                .run()
                .await;
        }
    });
}
