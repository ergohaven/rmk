//! Shared fork-related types used by firmware and protocol layers.

use core::ops::{BitAnd, BitOr, Not};

use bitfield_struct::bitfield;
use postcard::experimental::max_size::MaxSize;
use serde::{Deserialize, Serialize};

use crate::action::KeyAction;
use crate::led_indicator::LedIndicator;
use crate::modifier::ModifierCombination;
use crate::mouse_button::MouseButtons;

/// Bitset state used by fork matching logic.
///
/// A zero (default) value means "match nothing" — no modifiers, LEDs, or mouse buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, MaxSize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "wasm", derive(tsify::Tsify))]
#[cfg_attr(feature = "wasm", tsify(into_wasm_abi, from_wasm_abi))]
pub struct StateBits {
    /// Active modifier combination to match.
    pub modifiers: ModifierCombination,
    /// LED indicator state to match (Num/Caps/Scroll Lock, etc.).
    pub leds: LedIndicator,
    /// Mouse button state to match.
    pub mouse: MouseButtons,
}

impl BitOr for StateBits {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self {
            modifiers: self.modifiers | rhs.modifiers,
            leds: self.leds | rhs.leds,
            mouse: self.mouse | rhs.mouse,
        }
    }
}

impl BitAnd for StateBits {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self {
            modifiers: self.modifiers & rhs.modifiers,
            leds: self.leds & rhs.leds,
            mouse: self.mouse & rhs.mouse,
        }
    }
}

impl Not for StateBits {
    type Output = Self;

    fn not(self) -> Self::Output {
        Self {
            modifiers: !self.modifiers,
            leds: !self.leds,
            mouse: !self.mouse,
        }
    }
}

impl StateBits {
    pub const fn new_from(modifiers: ModifierCombination, leds: LedIndicator, mouse: MouseButtons) -> Self {
        Self { modifiers, leds, mouse }
    }
}

/// Vial/QMK key-override compatibility bits, stored as one byte whose bit
/// positions match the Vial `options` wire byte. RMK-native forks (TOML /
/// `Fork::new`) use the defaults, which keep RMK's own behavior: match any
/// `match_any` bit and latch the decision at trigger press until release.
/// Vial-created entries carry whatever the GUI configured.
#[bitfield(u8, order = Lsb, defmt = cfg(feature = "defmt"))]
#[derive(Eq, PartialEq, MaxSize)]
pub struct ForkOptions {
    /// The fork may take the positive branch when the trigger key is pressed.
    #[bits(1, default = true)]
    pub activate_on_trigger_down: bool,
    /// The fork may switch to the positive branch mid-hold when a required
    /// modifier is pressed while the trigger key is held.
    #[bits(1)]
    pub activate_on_required_mod_down: bool,
    /// The fork may switch to the positive branch mid-hold when a negative
    /// modifier is released while the trigger key is held.
    #[bits(1)]
    pub activate_on_negative_mod_up: bool,
    /// Any single `match_any` bit selects the positive branch (RMK's native
    /// matching, the default). When cleared, ALL `match_any` bits are
    /// required — QMK AND matching: a modifier kind required on both sides
    /// (e.g. LShift|RShift) is satisfied by either side, a kind required on
    /// one side needs exactly that side, and an empty `match_any` matches
    /// unconditionally.
    #[bits(1, default = true)]
    pub one_mod: bool,
    /// When the fork deactivates mid-hold, don't register the negative
    /// output in place of the positive one.
    #[bits(1)]
    pub no_reregister_trigger: bool,
    /// Don't deactivate the positive branch when another key is pressed.
    /// Defaults to true: forks latch their decision until the trigger is
    /// released (RMK's native behavior); Vial-created entries typically clear
    /// it (QMK's default deactivates on other key down).
    #[bits(1, default = true)]
    pub no_unregister_on_other_key_down: bool,
    #[bits(1)]
    _reserved: bool,
    /// Vial's per-entry enable toggle. Disabled forks keep their
    /// configuration but never fire.
    #[bits(1, default = true)]
    pub enabled: bool,
}

// u8 on the wire (postcard); named bools on serde-wasm-bindgen / serde_json (TS).
crate::bitfield_named_serde!(ForkOptions, "ForkOptions", {
    activate_on_trigger_down = with_activate_on_trigger_down,
    activate_on_required_mod_down = with_activate_on_required_mod_down,
    activate_on_negative_mod_up = with_activate_on_negative_mod_up,
    one_mod = with_one_mod,
    no_reregister_trigger = with_no_reregister_trigger,
    no_unregister_on_other_key_down = with_no_unregister_on_other_key_down,
    enabled = with_enabled,
});

impl ForkOptions {
    /// QMK key-override defaults: all activation events allowed, all trigger
    /// modifiers required, deactivates when another key is pressed down.
    pub const fn qmk_default() -> Self {
        Self::new()
            .with_activate_on_required_mod_down(true)
            .with_activate_on_negative_mod_up(true)
            .with_one_mod(false)
            .with_no_unregister_on_other_key_down(false)
    }

    /// Whether the fork re-evaluates its decision while the trigger is held.
    pub const fn reevaluates_mid_hold(&self) -> bool {
        self.activate_on_required_mod_down() || self.activate_on_negative_mod_up()
    }
}

/// Fork (key override) configuration.
///
/// A fork overrides a key's output based on the current modifier/LED/mouse state.
/// When the trigger key is pressed, the fork checks current state against `match_any`
/// and `match_none` to decide between `positive_output` and `negative_output`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "wasm", derive(tsify::Tsify))]
#[cfg_attr(feature = "wasm", tsify(into_wasm_abi, from_wasm_abi))]
pub struct Fork {
    /// The key action that activates this fork. Should not be `KeyAction::Transparent`.
    pub trigger: KeyAction,
    /// Output when the state condition is NOT met. `Transparent` leaves the key alone.
    pub negative_output: KeyAction,
    /// Output when the state condition IS met.
    pub positive_output: KeyAction,
    /// State bits selecting the positive branch (see `ForkOptions::one_mod`
    /// for how they are compared).
    pub match_any: StateBits,
    /// If any of these state bits are active, the fork is suppressed.
    pub match_none: StateBits,
    /// Modifiers removed from the HID report while the fork's decision is
    /// active (explicit modifiers pressed afterwards are still reported).
    pub suppressed_modifiers: ModifierCombination,
    /// Vial/QMK key-override compatibility bits (matching, activation,
    /// per-entry enable). RMK-native forks use the defaults.
    pub options: ForkOptions,
    /// Whether this fork's output is checked against other forks that have not
    /// yet triggered, allowing chained replacements.
    pub bindable: bool,
    /// Bitmask of layers the trigger must come from. `u32::MAX` means all layers.
    pub layers: u32,
}

impl Default for Fork {
    fn default() -> Self {
        Self::empty()
    }
}

impl Fork {
    pub fn new(
        trigger: KeyAction,
        negative_output: KeyAction,
        positive_output: KeyAction,
        match_any: StateBits,
        match_none: StateBits,
        suppressed_modifiers: ModifierCombination,
        bindable: bool,
    ) -> Self {
        Self {
            trigger,
            negative_output,
            positive_output,
            match_any,
            match_none,
            suppressed_modifiers,
            options: ForkOptions::default(),
            bindable,
            layers: u32::MAX,
        }
    }

    pub fn empty() -> Self {
        Self {
            options: ForkOptions::default().with_enabled(false),
            ..Self::new(
                KeyAction::No,
                KeyAction::No,
                KeyAction::No,
                StateBits::default(),
                StateBits::default(),
                ModifierCombination::default(),
                false,
            )
        }
    }

    /// Decide the fork's branch for the given live state: `true` selects
    /// `positive_output`. Does not include the layer gate.
    pub fn is_positive(&self, state: StateBits) -> bool {
        let matched = if self.options.one_mod() {
            (self.match_any & state) != StateBits::default()
        } else {
            state.modifiers.contains_all_paired(self.match_any.modifiers)
                && (state.leds & self.match_any.leds) == self.match_any.leds
                && (state.mouse & self.match_any.mouse) == self.match_any.mouse
        };
        matched && (self.match_none & state) == StateBits::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIFTS: ModifierCombination = ModifierCombination::from_bits(0x22); // LShift | RShift

    #[test]
    fn test_contains_all_paired() {
        // Empty requirement is always satisfied
        assert!(ModifierCombination::new().contains_all_paired(ModifierCombination::new()));
        assert!(ModifierCombination::LCTRL.contains_all_paired(ModifierCombination::new()));

        // A kind required on both sides accepts either side
        assert!(ModifierCombination::LSHIFT.contains_all_paired(SHIFTS));
        assert!(ModifierCombination::RSHIFT.contains_all_paired(SHIFTS));
        assert!(!ModifierCombination::LCTRL.contains_all_paired(SHIFTS));

        // A kind required on one side needs exactly that side
        assert!(!ModifierCombination::RSHIFT.contains_all_paired(ModifierCombination::LSHIFT));
        assert!(ModifierCombination::LSHIFT.contains_all_paired(ModifierCombination::LSHIFT));

        // Multiple kinds: every kind must be satisfied
        let ctrl_and_shifts = ModifierCombination::LCTRL | SHIFTS;
        assert!((ModifierCombination::LCTRL | ModifierCombination::RSHIFT).contains_all_paired(ctrl_and_shifts));
        assert!(!ModifierCombination::LCTRL.contains_all_paired(ctrl_and_shifts));

        // Extra active modifiers don't hurt
        assert!((ModifierCombination::LSHIFT | ModifierCombination::LALT).contains_all_paired(SHIFTS));
    }

    fn state(modifiers: ModifierCombination) -> StateBits {
        StateBits {
            modifiers,
            ..Default::default()
        }
    }

    #[test]
    fn test_is_positive_any() {
        let fork = Fork {
            match_any: state(SHIFTS),
            match_none: state(ModifierCombination::LALT),
            ..Fork::empty()
        };
        assert!(fork.is_positive(state(ModifierCombination::LSHIFT)));
        assert!(!fork.is_positive(state(ModifierCombination::LCTRL)));
        // match_none veto
        assert!(!fork.is_positive(state(ModifierCombination::LSHIFT | ModifierCombination::LALT)));
    }

    #[test]
    fn test_is_positive_all() {
        let all_mods = ForkOptions::default().with_one_mod(false);
        let fork = Fork {
            match_any: state(ModifierCombination::LCTRL | SHIFTS),
            options: all_mods,
            ..Fork::empty()
        };
        // Ctrl alone is not enough, Ctrl + either shift is
        assert!(!fork.is_positive(state(ModifierCombination::LCTRL)));
        assert!(fork.is_positive(state(ModifierCombination::LCTRL | ModifierCombination::RSHIFT)));

        // Empty match_any with one_mod cleared matches unconditionally
        let unconditional = Fork {
            options: all_mods,
            match_none: state(SHIFTS),
            ..Fork::empty()
        };
        assert!(unconditional.is_positive(state(ModifierCombination::new())));
        assert!(!unconditional.is_positive(state(ModifierCombination::RSHIFT)));
    }
}
