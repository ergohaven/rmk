use rmk_types::action::KeyAction;
use rmk_types::modifier::ModifierCombination;

use crate::event::KeyboardEvent;

#[derive(Copy, Clone, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ActiveFork {
    pub(crate) replacement: KeyAction, // the final replacement decision of the full fork chain
    pub(crate) suppress: ModifierCombination, // aggregate the chain's match_any modifiers here
    pub(crate) positive: bool,         // whether the current decision is the positive branch
    pub(crate) event: KeyboardEvent, // the trigger's press event: its pos matches the release, also used for mid-hold swaps
}

/// A planned mid-hold output swap for a held fork trigger.
pub(crate) struct ForkSwap {
    pub(crate) idx: usize,
    pub(crate) active: ActiveFork,
    pub(crate) output: KeyAction,
    pub(crate) suppress: ModifierCombination,
    pub(crate) match_any_mods: ModifierCombination,
    pub(crate) positive: bool,
}
