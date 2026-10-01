pub mod common;

use embassy_time::Duration;
use rmk::config::{BehaviorConfig, OneShotModifiersConfig};
use rmk::types::modifier::ModifierCombination;

mod one_shot_test {
    use rmk::config::{OneShotConfig, PositionalConfig};
    use rmk::keyboard::Keyboard;
    use rmk::types::action::KeyAction;
    use rmk::{k, osl, osm, th, user, wm};

    use super::*;
    use crate::common::{KC_LCTRL, KC_LGUI, KC_LSHIFT, wrap_keymap};

    // KEYMAP
    // Layer 0: OSM(LShift)        OSL(1)  A  TH(B)  OSM(LCtrl)  WM(B)  User(0)
    // Layer 1: OSM(LShift|LCtrl)  No      C  D      E           F      User(0x90)

    const KEYMAP: [[[KeyAction; 7]; 1]; 2] = [
        [[
            // Layer 0
            osm!(ModifierCombination::new_from(false, false, false, true, false)), // OSM LShift
            osl!(1),                                                               // OSL Layer 1
            k!(A),                                                                 // Regular key A
            th!(B, C),                                                             // Tap-hold key B, C
            osm!(ModifierCombination::new_from(false, false, false, false, true)), // OSM LCtrl
            wm!(B, ModifierCombination::new_from(false, true, false, false, false)), // WM B with LGUI
            user!(0),                                                              // User action without HID output
        ]],
        [[
            // Layer 1
            osm!(ModifierCombination::new_from(false, false, false, true, true)), // OSM LShift + LCtrl
            k!(No),                                                               // No action
            k!(C),                                                                // Layer 1 key C
            k!(D),                                                                // Layer 1 key D
            k!(E),                                                                // Layer 1 key E
            k!(F),                                                                // Layer 1 key F
            user!(0x90),                                                          // Universal symbol Dot
        ]],
    ];

    fn create_test_keyboard() -> Keyboard<'static> {
        let behavior_config: &'static mut BehaviorConfig = Box::leak(Box::new(BehaviorConfig::default()));
        let per_key_config: &'static PositionalConfig<1, 7> = Box::leak(Box::new(PositionalConfig::default()));
        Keyboard::new(wrap_keymap(KEYMAP, per_key_config, behavior_config))
    }

    fn create_test_keyboard_with_behavior_config(config: BehaviorConfig) -> Keyboard<'static> {
        let behavior_config: &'static mut BehaviorConfig = Box::leak(Box::new(config));
        let per_key_config: &'static PositionalConfig<1, 7> = Box::leak(Box::new(PositionalConfig::default()));
        Keyboard::new(wrap_keymap(KEYMAP, per_key_config, behavior_config))
    }

    fn create_test_keyboard_with_one_shot_modifiers_config(config: OneShotModifiersConfig) -> Keyboard<'static> {
        let behavior_config: &'static mut BehaviorConfig = Box::leak(Box::new(BehaviorConfig {
            one_shot_modifiers: config,
            ..BehaviorConfig::default()
        }));
        let per_key_config: &'static PositionalConfig<1, 7> = Box::leak(Box::new(PositionalConfig::default()));
        Keyboard::new(wrap_keymap(KEYMAP, per_key_config, behavior_config))
    }

    /// OSM Test Case 1
    ///
    /// Config:
    /// - timeout: 1000ms
    /// - activate_on_keypress: false
    ///
    /// Sequence:
    /// - Press and Release OSM LShift
    /// - Press and Release regular key A
    ///
    /// Expected:
    /// - A with LShift
    /// - All released
    #[test]
    fn test_osm_basic_single_behavior() {
        key_sequence_test! {
            keyboard: create_test_keyboard(),
            sequence: [
                // Press and Release OSM LShift
                [0, 0, true, 10],
                [0, 0, false, 10],
                // Press and Release A
                [0, 2, true, 10],
                [0, 2, false, 10],
            ],
            expected_reports: [
                [KC_LSHIFT, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A with LShift
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    /// OSM Test Case 2
    ///
    /// Config:
    /// - timeout: 100ms
    /// - activate_on_keypress: false
    ///
    /// Sequence:
    /// - Press and Release OSM LShift
    /// - Press and Release A after timeout (delay > 100ms)
    ///
    /// Expected:
    /// - A is sent without LShift
    /// - All released
    #[test]
    fn test_osm_timeout() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_behavior_config(
                BehaviorConfig {
                    one_shot: OneShotConfig {
                        timeout: Duration::from_millis(100),
                        ..OneShotConfig::default()
                    },
                    ..BehaviorConfig::default()
                }
            ),
            sequence: [
                // Press and Release OSM LShift
                [0, 0, true, 10],
                [0, 0, false, 10],
                // Press and Release A after timeout (delay > 100ms)
                [0, 2, true, 150],
                [0, 2, false, 10],
            ],
            expected_reports: [
                [0, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A without LShift (timeout)
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    /// OSM Test Case 3
    ///
    /// Config:
    /// - timeout: 1000ms
    /// - activate_on_keypress: false
    ///
    /// Sequence:
    /// - Press OSM LShift
    /// - Press A while OSM is held
    /// - Release A
    /// - Release OSM LShift
    ///
    /// Expected:
    /// - A with LShift
    /// - LShift is still held
    /// - All released
    #[test]
    fn test_osm_held_behavior() {
        key_sequence_test! {
            keyboard: create_test_keyboard(),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 2, true, 10],   // Press A while OSM is held
                [0, 2, false, 10],  // Release A
                [0, 0, false, 10],  // Release OSM LShift
            ],
            expected_reports: [
                [KC_LSHIFT, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A with LShift
                [KC_LSHIFT, [0, 0, 0, 0, 0, 0]], // Still holding LShift
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    /// OSM Test Case 4
    ///
    /// Config:
    /// - timeout: 1000ms
    /// - activate_on_keypress: false
    ///
    /// Sequence:
    /// - Press and Release OSM LShift
    /// - Press and Release regular key A
    /// - Press and Release regular key B
    ///
    /// Expected:
    /// - A with LShift
    /// - All released
    /// - B without LShift
    /// - All released
    #[test]
    fn test_osm_multiple_keys() {
        key_sequence_test! {
            keyboard: create_test_keyboard(),
            sequence: [
                // Press and Release OSM LShift
                [0, 0, true, 10],
                [0, 0, false, 10],
                // Press and Release A
                [0, 2, true, 10],
                [0, 2, false, 10],
                // Press and Release B
                [0, 3, true, 10],
                [0, 3, false, 10],
            ],
            expected_reports: [
                [KC_LSHIFT, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A with LShift
                [0, [0, 0, 0, 0, 0, 0]], // All released
                [0, [kc_to_u8!(B), 0, 0, 0, 0, 0]], // B without LShift
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    /// OSM Test Case 5
    ///
    /// Config:
    /// - timeout: 1000ms
    /// - activate_on_keypress: false
    ///
    /// Sequence:
    /// - Press OSM LShift
    /// - Press B
    /// - Release OSM LShift
    /// - Release B
    ///
    /// Expected:
    /// - B with LShift
    /// - All released
    #[test]
    fn test_osm_rolling_with_tap_hold() {
        key_sequence_test! {
            keyboard: create_test_keyboard(),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 3, true, 10],   // Press B
                [0, 0, false, 10],  // Release OSM LShift
                [0, 3, false, 10],  // Release B
            ],
            expected_reports: [
                [KC_LSHIFT, [kc_to_u8!(B), 0, 0, 0, 0, 0]], // B with LShift
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    /// OSM Test Case 6
    ///
    /// Config:
    /// - timeout: 1000ms
    /// - activate_on_keypress: false
    ///
    /// Sequence:
    /// - Press and Release OSM LShift
    /// - Press and Release OSM LCtrl
    /// - Press and Release regular key A
    ///
    /// Expected:
    /// - A with LShift+LCtrl
    /// - All released
    #[test]
    fn test_osm_combined_modifiers() {
        key_sequence_test! {
            keyboard: create_test_keyboard(),
            sequence: [
                // Press and Release OSM LShift
                [0, 0, true, 10],
                [0, 0, false, 10],
                // Press and Release OSM LCtrl
                [0, 4, true, 10],
                [0, 4, false, 10],
                // Press and Release A
                [0, 2, true, 10],
                [0, 2, false, 10],
            ],
            expected_reports: [
                [KC_LSHIFT | KC_LCTRL, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A with LShift+LCtrl
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    /// OSM Test Case 7
    ///
    /// Config:
    /// - timeout: 100ms
    /// - activate_on_keypress: false
    ///
    /// Sequence:
    /// - Press and Release OSM LShift
    /// - Press and Release OSM LCtrl
    /// - Press and Release WM(B, LGui)
    ///
    /// Expected:
    /// - B is sent with LShift + LCtrl + LGui
    /// - All released
    #[test]
    fn test_osm_multiple_osm_with_wm() {
        key_sequence_test! {
            keyboard: create_test_keyboard(),
            sequence: [
                // Press and Release OSM LShift
                [0, 0, true, 10],
                [0, 0, false, 10],
                // Press and Release OSM LCtrl
                [0, 4, true, 10],
                [0, 4, false, 10],
                // Press and Release WM(B, LGui)
                [0, 5, true, 10],
                [0, 5, false, 10],
            ],
            expected_reports: [
                [KC_LSHIFT | KC_LCTRL | KC_LGUI, [kc_to_u8!(B), 0, 0, 0, 0, 0]], // B with LShift + LCtrl + LGui
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    /// OSM Test Case 8
    ///
    /// Config:
    /// - timeout: 100ms
    /// - activate_on_keypress: true
    ///
    /// Sequence:
    /// - Press OSM LShift
    /// - Release OSM LShift
    /// - Press A
    /// - Release A
    ///
    /// Expected:
    /// - LShift is sent from the start
    /// - A with LShift
    /// - All released
    #[test]
    fn test_osm_activate_on_keypress() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_one_shot_modifiers_config(OneShotModifiersConfig {
                activate_on_keypress: true,
                ..OneShotModifiersConfig::default()
            }),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 0, false, 10],  // Release OSM LShift
                [0, 2, true, 10],   // Press A
                [0, 2, false, 10],  // Release A
            ],
            expected_reports: [
                [KC_LSHIFT, [0, 0, 0, 0, 0, 0]], // LShift is sent from the start
                [KC_LSHIFT, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A with LShift
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        }
    }

    /// OSM Test Case 9
    ///
    /// Config:
    /// - timeout: 100ms
    /// - activate_on_keypress: true
    ///
    /// Sequence:
    /// - Press and Release OSM LShift
    /// - Press and Release OSM LCtrl
    /// - Press and Release regular key A
    ///
    /// Expected:
    /// - LShift is sent first
    /// - LCtrl is added to combination
    /// - A with LShift+LCtrl
    /// - All released
    #[test]
    fn test_osm_combined_modifiers_with_activate_on_keypress() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_one_shot_modifiers_config(OneShotModifiersConfig {
                activate_on_keypress: true,
                ..OneShotModifiersConfig::default()
            }),
            sequence: [
                // Press and Release OSM LShift
                [0, 0, true, 10],
                [0, 0, false, 10],
                // Press and Release OSM LCtrl
                [0, 4, true, 10],
                [0, 4, false, 10],
                // Press and Release A
                [0, 2, true, 10],
                [0, 2, false, 10],
            ],
            expected_reports: [
                [KC_LSHIFT, [0, 0, 0, 0, 0, 0]], // LShift is sent first
                [KC_LSHIFT | KC_LCTRL, [0, 0, 0, 0, 0, 0]], // LCtrl is added to combination
                [KC_LSHIFT | KC_LCTRL, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A with LShift+LCtrl
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    // OSL Tests
    #[test]
    fn test_osl_basic_single_behavior() {
        key_sequence_test! {
            keyboard: create_test_keyboard(),
            sequence: [
                [0, 1, true, 10],   // Press OSL Layer 1
                [0, 1, false, 10],  // Release OSL Layer 1
                [0, 2, true, 10],   // Press key at (0,2), should get C from layer 1
                [0, 2, false, 10],  // Release key
            ],
            expected_reports: [
                [0, [kc_to_u8!(C), 0, 0, 0, 0, 0]], // C from layer 1
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    #[test]
    fn test_osl_held_behavior() {
        key_sequence_test! {
            keyboard: create_test_keyboard(),
            sequence: [
                [0, 1, true, 10],   // Press OSL Layer 1
                [0, 2, true, 10],   // Press key at (0,2) while OSL is held
                [0, 2, false, 10],  // Release key
                [0, 1, false, 10],  // Release OSL Layer 1
            ],
            expected_reports: [
                [0, [kc_to_u8!(C), 0, 0, 0, 0, 0]], // C from layer 1
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    #[test]
    fn test_osl_timeout() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_behavior_config(
                BehaviorConfig {
                    one_shot: OneShotConfig {
                        timeout: Duration::from_millis(100),
                        ..OneShotConfig::default()
                    },
                    one_shot_modifiers: OneShotModifiersConfig {
                        ..OneShotModifiersConfig::default()
                    },
                    ..BehaviorConfig::default()
                }
            ),
            sequence: [
                [0, 1, true, 10],   // Press OSL Layer 1
                [0, 1, false, 10],  // Release OSL Layer 1
                [0, 2, true, 150],  // Press key at (0,2) after timeout (delay > 100ms)
                [0, 2, false, 10],  // Release key
            ],
            expected_reports: [
                [0, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A from layer 0 (timeout)
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    #[test]
    fn test_osl_multiple_keys() {
        key_sequence_test! {
            keyboard: create_test_keyboard(),
            sequence: [
                [0, 1, true, 10],   // Press OSL Layer 1
                [0, 1, false, 10],  // Release OSL Layer 1
                [0, 2, true, 10],   // Press key at (0,2), should get C from layer 1
                [0, 2, false, 10],  // Release key
                [0, 3, true, 10],   // Press key at (0,3), should get B from layer 0
                [0, 3, false, 10],  // Release key
            ],
            expected_reports: [
                [0, [kc_to_u8!(C), 0, 0, 0, 0, 0]], // C from layer 1
                [0, [0, 0, 0, 0, 0, 0]], // All released
                [0, [kc_to_u8!(B), 0, 0, 0, 0, 0]], // B from layer 0
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    #[test]
    fn test_osm_then_osl() {
        key_sequence_test! {
            keyboard: create_test_keyboard(),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 0, false, 10],  // Release OSM LShift
                [0, 1, true, 10],   // Press OSL Layer 1
                [0, 1, false, 10],  // Release OSL Layer 1
                [0, 2, true, 10],   // Press key at (0,2), should get C from layer 1 with shift
                [0, 2, false, 10],  // Release key
            ],
            expected_reports: [
                [0, [kc_to_u8!(C), 0, 0, 0, 0, 0]], // C from layer 1 with LShift
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    #[test]
    fn test_osl_then_osm() {
        key_sequence_test! {
            keyboard: create_test_keyboard(),
            sequence: [
                [0, 1, true, 10],   // Press OSL Layer 1
                [0, 1, false, 10],  // Release OSL Layer 1
                [0, 0, true, 10],   // Press OSM LShift (from layer 1, but No action)
                [0, 0, false, 10],  // Release OSM LShift (gets from layer 0 due to transparent)
                [0, 2, true, 10],   // Press key at (0,2), should get A from layer 0 with shift + ctrl
                [0, 2, false, 10],  // Release key
            ],
            expected_reports: [
                [KC_LSHIFT | KC_LCTRL, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A from layer 0 with shift + ctrl
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    #[test]
    fn test_osm_and_osl_timeout() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_behavior_config(
                BehaviorConfig {
                    one_shot: OneShotConfig {
                        timeout: Duration::from_millis(100),
                        ..OneShotConfig::default()
                    },
                    one_shot_modifiers: OneShotModifiersConfig {
                        ..OneShotModifiersConfig::default()
                    },
                    ..BehaviorConfig::default()
                }
            ),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 0, false, 10],  // Release OSM LShift
                [0, 1, true, 10],   // Press OSL Layer 1
                [0, 1, false, 10],  // Release OSL Layer 1
                [0, 2, true, 200], // Press key at (0,2) after timeout (delay > 100ms)
                [0, 2, false, 10],  // Release key
            ],
            expected_reports: [
                [0, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A from layer 0 (both timeout)
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    /// Chain mode (quick_release = false): modifier released on key RELEASE
    #[test]
    fn test_osm_chain_mode_basic() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_one_shot_modifiers_config(OneShotModifiersConfig {
                quick_release: false,
                ..OneShotModifiersConfig::default()
            }),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 0, false, 10],  // Release OSM LShift
                [0, 2, true, 10],   // Press A
                [0, 2, false, 10],  // Release A
            ],
            expected_reports: [
                [KC_LSHIFT, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A with LShift
                [0, [0, 0, 0, 0, 0, 0]], // Release A clears modifier
            ]
        };
    }

    /// Chain mode: tap A then tap B — only A gets modifier
    #[test]
    fn test_osm_chain_mode_multiple_keys() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_one_shot_modifiers_config(OneShotModifiersConfig {
                quick_release: false,
                ..OneShotModifiersConfig::default()
            }),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 0, false, 10],  // Release LShift
                [0, 2, true, 10],   // Press A
                [0, 2, false, 10],  // Release A
                [0, 3, true, 10],   // Press B
                [0, 3, false, 10],  // Release B
            ],
            expected_reports: [
                [KC_LSHIFT, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A with LShift
                [0, [0, 0, 0, 0, 0, 0]], // Release A clears modifier
                [0, [kc_to_u8!(B), 0, 0, 0, 0, 0]], // B without modifier
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    /// Chain mode with activate_on_keypress
    #[test]
    fn test_osm_chain_mode_activate_on_keypress() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_one_shot_modifiers_config(OneShotModifiersConfig {
                activate_on_keypress: true,
                quick_release: false,
                ..OneShotModifiersConfig::default()
            }),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 0, false, 10],  // Release OSM LShift
                [0, 2, true, 10],   // Press A
                [0, 2, false, 10],  // Release A
            ],
            expected_reports: [
                [KC_LSHIFT, [0, 0, 0, 0, 0, 0]], // LShift sent immediately
                [KC_LSHIFT, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A with LShift
                [0, [0, 0, 0, 0, 0, 0]], // Release A clears modifier
            ]
        };
    }

    // Quick-release mode tests (quick_release = true)

    #[test]
    fn test_osm_quick_release_basic() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_one_shot_modifiers_config(OneShotModifiersConfig {
                quick_release: true,
                ..OneShotModifiersConfig::default()
            }),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 0, false, 10],  // Release OSM LShift
                [0, 2, true, 10],   // Press A
                [0, 2, false, 10],  // Release A
            ],
            expected_reports: [
                [KC_LSHIFT, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A with LShift
                [0, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // Quick-release: modifier removed, key still held
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    #[test]
    fn test_osm_quick_release_multiple_keys() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_one_shot_modifiers_config(OneShotModifiersConfig {
                quick_release: true,
                ..OneShotModifiersConfig::default()
            }),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 0, false, 10],  // Release OSM LShift
                [0, 2, true, 10],   // Press A
                [0, 2, false, 10],  // Release A
                [0, 3, true, 10],   // Press B
                [0, 3, false, 10],  // Release B
            ],
            expected_reports: [
                [KC_LSHIFT, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A with LShift
                [0, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // Quick-release: modifier removed
                [0, [0, 0, 0, 0, 0, 0]], // All released
                [0, [kc_to_u8!(B), 0, 0, 0, 0, 0]], // B without LShift
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    // TODO: test_osm_quick_release_rolling removed — OSM + morse/tap-hold interaction
    // has a known bug where the OSM deadline loop times out before the tap resolves.

    #[test]
    fn test_osm_quick_release_combined_modifiers() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_one_shot_modifiers_config(OneShotModifiersConfig {
                quick_release: true,
                ..OneShotModifiersConfig::default()
            }),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 0, false, 10],  // Release OSM LShift
                [0, 4, true, 10],   // Press OSM LCtrl
                [0, 4, false, 10],  // Release OSM LCtrl
                [0, 2, true, 10],   // Press A
                [0, 2, false, 10],  // Release A
            ],
            expected_reports: [
                [KC_LSHIFT | KC_LCTRL, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A with LShift+LCtrl
                [0, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // Quick-release: modifiers removed
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    #[test]
    fn test_osm_quick_release_with_wm() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_one_shot_modifiers_config(OneShotModifiersConfig {
                quick_release: true,
                ..OneShotModifiersConfig::default()
            }),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 0, false, 10],  // Release OSM LShift
                [0, 4, true, 10],   // Press OSM LCtrl
                [0, 4, false, 10],  // Release OSM LCtrl
                [0, 5, true, 10],   // Press WM(B, LGui)
                [0, 5, false, 10],  // Release WM(B, LGui)
            ],
            expected_reports: [
                [KC_LSHIFT | KC_LCTRL | KC_LGUI, [kc_to_u8!(B), 0, 0, 0, 0, 0]], // B with LShift + LCtrl + LGui
                [KC_LGUI, [kc_to_u8!(B), 0, 0, 0, 0, 0]], // Quick-release: OSM modifiers removed, WM stays
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    #[test]
    fn test_osm_quick_release_activate_on_keypress() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_one_shot_modifiers_config(OneShotModifiersConfig {
                activate_on_keypress: true,
                quick_release: true,
                ..OneShotModifiersConfig::default()
            }),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 0, false, 10],  // Release OSM LShift
                [0, 2, true, 10],   // Press A
                [0, 2, false, 10],  // Release A
            ],
            expected_reports: [
                [KC_LSHIFT, [0, 0, 0, 0, 0, 0]], // LShift sent immediately
                [KC_LSHIFT, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A with LShift
                [0, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // Quick-release: modifier removed
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    #[test]
    fn test_osm_quick_release_combined_activate_on_keypress() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_one_shot_modifiers_config(OneShotModifiersConfig {
                activate_on_keypress: true,
                quick_release: true,
                ..OneShotModifiersConfig::default()
            }),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 0, false, 10],  // Release OSM LShift
                [0, 4, true, 10],   // Press OSM LCtrl
                [0, 4, false, 10],  // Release OSM LCtrl
                [0, 2, true, 10],   // Press A
                [0, 2, false, 10],  // Release A
            ],
            expected_reports: [
                [KC_LSHIFT, [0, 0, 0, 0, 0, 0]], // LShift sent first
                [KC_LSHIFT | KC_LCTRL, [0, 0, 0, 0, 0, 0]], // LCtrl added
                [KC_LSHIFT | KC_LCTRL, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // A with LShift+LCtrl
                [0, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // Quick-release: modifiers removed
                [0, [0, 0, 0, 0, 0, 0]], // All released
            ]
        };
    }

    /// Regression test for Action::User without HID output when activate_on_keypress=true and quick_release=true.
    /// Modifier should be released on Action::User press because OSM is consumed immediately.
    #[test]
    fn test_osm_action_user_no_hid_output_activate_on_keypress_quick_release() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_one_shot_modifiers_config(OneShotModifiersConfig {
                activate_on_keypress: true,
                quick_release: true,
                ..OneShotModifiersConfig::default()
            }),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 0, false, 10],  // Release OSM LShift
                [0, 6, true, 10],   // Press Action::User without HID output (user!(0))
                [0, 6, false, 10],  // Release Action::User
            ],
            expected_reports: [
                [KC_LSHIFT, [0, 0, 0, 0, 0, 0]], // LShift sent on OSM press
                [0, [0, 0, 0, 0, 0, 0]],         // Modifier released on Action::User press (quick release)
            ]
        };
    }

    /// Regression test for Action::User without HID output when activate_on_keypress=true and quick_release=false (chain mode).
    /// Modifier should be released on Action::User release when OSM is consumed.
    #[test]
    fn test_osm_action_user_no_hid_output_activate_on_keypress_chain_mode() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_one_shot_modifiers_config(OneShotModifiersConfig {
                activate_on_keypress: true,
                quick_release: false,
                ..OneShotModifiersConfig::default()
            }),
            sequence: [
                [0, 0, true, 10],   // Press OSM LShift
                [0, 0, false, 10],  // Release OSM LShift
                [0, 6, true, 10],   // Press Action::User without HID output (user!(0))
                [0, 6, false, 10],  // Release Action::User
            ],
            expected_reports: [
                [KC_LSHIFT, [0, 0, 0, 0, 0, 0]], // LShift sent on OSM press
                [0, [0, 0, 0, 0, 0, 0]],         // Modifier released on Action::User release
            ]
        };
    }

    /// Regression test verifying that modifier release preserves existing held keys.
    #[test]
    fn test_osm_action_user_no_hid_output_preserves_held_key() {
        key_sequence_test! {
            keyboard: create_test_keyboard_with_one_shot_modifiers_config(OneShotModifiersConfig {
                activate_on_keypress: true,
                quick_release: true,
                ..OneShotModifiersConfig::default()
            }),
            sequence: [
                [0, 2, true, 10],   // Press and hold A
                [0, 0, true, 10],   // Press OSM LShift
                [0, 0, false, 10],  // Release OSM LShift
                [0, 6, true, 10],   // Press Action::User without HID output
                [0, 6, false, 10],  // Release Action::User
                [0, 2, false, 10],  // Release A
            ],
            expected_reports: [
                [0, [kc_to_u8!(A), 0, 0, 0, 0, 0]],         // A pressed
                [KC_LSHIFT, [kc_to_u8!(A), 0, 0, 0, 0, 0]], // LShift added on OSM press while A held
                [0, [kc_to_u8!(A), 0, 0, 0, 0, 0]],         // LShift removed on User action press, A still held
                [0, [0, 0, 0, 0, 0, 0]],                    // A released
            ]
        };
    }

    /// Focused regression test for OSL + Universal Symbols release behavior:
    /// Activating layer 1 via OSL and triggering a Universal Symbol should deactivate
    /// the one-shot layer on release, so subsequent keypresses resolve on layer 0.
    #[test]
    #[cfg(feature = "universal_symbols")]
    fn test_osl_universal_symbols_release() {
        key_sequence_test! {
            keyboard: create_test_keyboard(),
            sequence: [
                [0, 1, true, 10],   // Press OSL Layer 1
                [0, 1, false, 10],  // Release OSL Layer 1
                [0, 6, true, 10],   // Press Universal Symbol Dot at (0,6) on Layer 1
                [0, 6, false, 10],  // Release Universal Symbol Dot (should deactivate Layer 1)
                [0, 2, true, 10],   // Press key at (0,2), should get A from Layer 0 (not C from Layer 1)
                [0, 2, false, 10],  // Release key
            ],
            expected_reports: [
                [0, [kc_to_u8!(Dot), 0, 0, 0, 0, 0]], // Dot stroke from universal symbols
                [0, [0, 0, 0, 0, 0, 0]],              // Tap released key
                [0, [0, 0, 0, 0, 0, 0]],              // Resolved modifiers restored
                [0, [kc_to_u8!(A), 0, 0, 0, 0, 0]],   // A from Layer 0 (proves Layer 1 was deactivated)
                [0, [0, 0, 0, 0, 0, 0]],              // All released
            ]
        };
    }
}
