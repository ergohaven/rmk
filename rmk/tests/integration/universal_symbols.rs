//! Universal Symbols keys, which a scenario cannot spell: `keyboard.toml`
//! numbers User keys 0-63, and the symbols start at `User(0x90)`.

use rmk::test_support::test_block_on;
use rmk::types::keycode::HidKeyCode;
use rmk::{a, k, osl, user};

use crate::simulator::SimKeyboard;

// A symbol typed under a one-shot layer spends that layer, so the next key
// resolves on layer 0. Regression: User keys never released the OSL, and the
// key after the symbol still came from layer 1.
#[test]
fn universal_symbol_spends_the_one_shot_layer() {
    test_block_on(async {
        let mut keyboard = SimKeyboard::builder([
            [[osl!(1), a!(No), k!(A)]],
            [[a!(No), user!(0x90), k!(C)]], // Dot
        ])
        .build()
        .await;
        keyboard
            .tap(0, 0, 10)
            .tap(0, 1, 10)
            .tap(0, 2, 10)
            .expect_keys([HidKeyCode::Dot])
            .expect_keys([])
            .expect_keys([]) // The symbol tap restores the resolved modifiers
            .expect_keys([HidKeyCode::A])
            .expect_keys([])
            .run()
            .await;
    });
}
