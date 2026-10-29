//! Shared façade for host-facing services (Vial today, rmk_protocol next).
//!
//! Bundles every keymap mutation with its flash persistence so callers don't
//! repeat storage plumbing by hand. Key positions use a last-write-wins
//! staging area so the serial host service never waits for flash capacity.
//! The façade also exposes synchronous reads of live keyboard state (LED,
//! battery, connection, active layer) that are otherwise scattered across
//! module-private statics.
//!
//! The context does not subscribe to events — the underlying statics it
//! reads from are kept in sync by the relevant event handlers
//! (`BatteryProcessor::commit`, `state.rs::update_status`,
//! `keyboard::run_led_reader`).

use embassy_time::Duration;
use rmk_types::action::{EncoderAction, KeyAction};
#[cfg(feature = "_ble")]
use rmk_types::battery::BatteryStatus;
use rmk_types::combo::Combo as ComboConfig;
use rmk_types::connection::ConnectionStatus;
use rmk_types::fork::Fork;
use rmk_types::led_indicator::LedIndicator;
use rmk_types::morse::{Morse, MorseProfile};

#[cfg(feature = "storage")]
use crate::channel::MACRO_FLASH_SIGNAL;
use crate::event::KeyboardEventPos;
use crate::keyboard::combo::Combo;
use crate::keymap::KeyMap;
#[cfg(feature = "storage")]
use crate::{
    channel::FLASH_CHANNEL,
    storage::{FlashOperationMessage, queue_keymap_flash_write},
};

/// Façade shared between Vial and rmk_protocol host services.
///
/// `keymap` is intentionally `pub`: callers like `VialLock` that only need a
/// raw `&KeyMap` keep their existing parameter and read it through
/// `ctx.keymap` at the construction site.
pub struct KeyboardContext<'a> {
    pub keymap: &'a KeyMap<'a>,
}

impl<'a> KeyboardContext<'a> {
    pub fn new(keymap: &'a KeyMap<'a>) -> Self {
        Self { keymap }
    }

    // ── Keymap operations ────────────────────────────────────────────────

    pub fn get_action(&self, layer: u8, row: u8, col: u8) -> KeyAction {
        self.keymap
            .get_action_at(KeyboardEventPos::key_pos(col, row), layer as usize)
    }

    pub fn get_action_flat(&self, index: usize) -> KeyAction {
        self.keymap.get_action_by_flat_index(index)
    }

    /// `(rows, cols, num_layers)`.
    pub fn keymap_dimensions(&self) -> (usize, usize, usize) {
        self.keymap.get_keymap_config()
    }

    pub async fn set_action(&self, layer: u8, row: u8, col: u8, action: KeyAction) {
        self.keymap
            .set_action_at(KeyboardEventPos::key_pos(col, row), layer as usize, action);
        #[cfg(feature = "storage")]
        if !queue_keymap_flash_write(layer, row, col, action) {
            error!("Failed to stage keymap key at layer {} ({},{})", layer, row, col);
        }
    }

    /// Synchronous on purpose: Vial's bulk-write path (`DynamicKeymapSetBuffer`)
    /// calls this in a tight loop. Persistence is staged in the key-position
    /// coalescer, so a full flash channel cannot drop the update or block Vial.
    ///
    /// `rows` / `cols` are passed in so callers can hoist the dimensions read
    /// out of their loop — see `keymap_dimensions()`.
    pub fn try_set_action_flat(&self, index: usize, action: KeyAction, rows: usize, cols: usize) {
        self.keymap.set_action_by_flat_index(index, action);
        #[cfg(feature = "storage")]
        {
            let (row, col, layer) = position_from_flat_index(index, rows, cols);
            if !queue_keymap_flash_write(layer as u8, row as u8, col as u8, action) {
                error!("Failed to stage keymap key at layer {} ({},{})", layer, row, col);
            }
        }
        #[cfg(not(feature = "storage"))]
        let _ = (rows, cols);
    }

    // ── Encoders ─────────────────────────────────────────────────────────

    pub fn get_encoder(&self, layer: u8, idx: u8) -> Option<EncoderAction> {
        self.keymap.get_encoder_action(layer as usize, idx as usize)
    }

    pub async fn set_encoder_clockwise(&self, layer: u8, idx: u8, action: KeyAction) {
        let updated = self.keymap.set_encoder_clockwise(layer as usize, idx as usize, action);
        #[cfg(feature = "storage")]
        if let Some(encoder) = updated {
            FLASH_CHANNEL
                .send(FlashOperationMessage::Encoder {
                    idx,
                    layer,
                    action: encoder,
                })
                .await;
        }
        #[cfg(not(feature = "storage"))]
        let _ = updated;
    }

    pub async fn set_encoder_counter_clockwise(&self, layer: u8, idx: u8, action: KeyAction) {
        let updated = self
            .keymap
            .set_encoder_counter_clockwise(layer as usize, idx as usize, action);
        #[cfg(feature = "storage")]
        if let Some(encoder) = updated {
            FLASH_CHANNEL
                .send(FlashOperationMessage::Encoder {
                    idx,
                    layer,
                    action: encoder,
                })
                .await;
        }
        #[cfg(not(feature = "storage"))]
        let _ = updated;
    }

    // ── Macros ───────────────────────────────────────────────────────────

    pub fn read_macro_buffer(&self, offset: usize, target: &mut [u8]) {
        self.keymap.read_macro_buffer(offset, target);
    }

    /// Update the live macro buffer and queue a flash snapshot only after the
    /// complete set of macro slots has arrived. Vial GUI and Entropy both send
    /// all slots as NUL-terminated bytecode, including empty trailing slots.
    pub fn write_macro_buffer(&self, offset: usize, data: &[u8], macro_count: usize) -> bool {
        self.keymap.write_macro_buffer(offset, data);
        let received_end = (offset + data.len()).min(crate::MACRO_SPACE_SIZE);
        let transfer_complete = received_end == crate::MACRO_SPACE_SIZE
            || self.keymap.macro_buffer_has_terminators(received_end, macro_count);
        #[cfg(feature = "storage")]
        if transfer_complete {
            MACRO_FLASH_SIGNAL.signal(self.keymap.get_macro_sequences());
            info!("Queue completed macro snapshot for storage");
        }
        transfer_complete
    }

    pub fn reset_macro_buffer(&self) {
        self.keymap.reset_macro_buffer();
    }

    // ── Combos ───────────────────────────────────────────────────────────

    pub fn with_combos<R>(&self, f: impl FnOnce(&[Option<Combo>]) -> R) -> R {
        self.keymap.with_combos(f)
    }

    /// Replace the combo at `idx` with `config` (or remove it if actions,
    /// output, and layer are all empty) and persist. No-op if `idx` is out of range.
    pub async fn set_combo(&self, idx: u8, config: ComboConfig) {
        let valid = self.keymap.with_combos_mut(|combos| {
            if (idx as usize) >= combos.len() {
                return false;
            }
            combos[idx as usize] =
                if config.actions.is_empty() && config.output == KeyAction::No && config.layer.is_none() {
                    None
                } else {
                    Some(Combo::new(config.clone()))
                };
            true
        });
        if !valid {
            return;
        }
        #[cfg(feature = "storage")]
        FLASH_CHANNEL.send(FlashOperationMessage::Combo { idx, config }).await;
        #[cfg(not(feature = "storage"))]
        let _ = config;
    }

    // ── Forks (Vial: key override) ───────────────────────────────────────

    pub fn with_forks<R>(&self, f: impl FnOnce(&[Fork]) -> R) -> R {
        self.keymap.with_forks(f)
    }

    /// Replace the fork at `idx` with `fork` and persist. No-op if `idx` is
    /// out of range.
    pub async fn set_fork(&self, idx: u8, fork: Fork) {
        let valid = self.keymap.with_forks_mut(|forks| {
            if (idx as usize) >= forks.len() {
                return false;
            }
            forks[idx as usize] = fork;
            true
        });
        if !valid {
            return;
        }
        #[cfg(feature = "storage")]
        FLASH_CHANNEL.send(FlashOperationMessage::Fork { idx, fork }).await;
    }

    // ── Morses (Vial: tap-dance) ─────────────────────────────────────────

    pub fn get_morse(&self, idx: u8) -> Option<Morse> {
        self.keymap.get_morse(idx as usize)
    }

    pub fn morses_len(&self) -> usize {
        self.keymap.morses_len()
    }

    /// Mutate the morse at `idx` and persist. No-op if `idx` is out of range.
    pub async fn update_morse(&self, idx: u8, f: impl FnOnce(&mut Morse)) {
        #[cfg(feature = "storage")]
        {
            let updated = self.keymap.with_morse_mut(idx as usize, |morse| {
                f(morse);
                morse.clone()
            });
            if let Some(morse) = updated {
                FLASH_CHANNEL.send(FlashOperationMessage::Morse { idx, morse }).await;
            }
        }
        #[cfg(not(feature = "storage"))]
        {
            self.keymap.with_morse_mut(idx as usize, f);
        }
    }

    // ── Behavior settings (read) ─────────────────────────────────────────

    pub fn combo_timeout(&self) -> Duration {
        self.keymap.combo_timeout()
    }

    pub fn one_shot_timeout(&self) -> Duration {
        self.keymap.one_shot_timeout()
    }

    pub fn tap_interval(&self) -> u16 {
        self.keymap.tap_interval()
    }

    pub fn tap_capslock_interval(&self) -> u16 {
        self.keymap.tap_capslock_interval()
    }

    pub fn morse_default_profile(&self) -> MorseProfile {
        self.keymap.morse_default_profile()
    }

    pub fn morse_prior_idle_time(&self) -> Duration {
        self.keymap.morse_prior_idle_time()
    }

    // ── Behavior settings (write+persist) ────────────────────────────────

    pub async fn set_combo_timeout(&self, ms: u16) {
        self.keymap.set_combo_timeout(Duration::from_millis(ms as u64));
        #[cfg(feature = "storage")]
        FLASH_CHANNEL.send(FlashOperationMessage::ComboTimeout(ms)).await;
    }

    pub async fn set_one_shot_timeout(&self, ms: u16) {
        self.keymap.set_one_shot_timeout(Duration::from_millis(ms as u64));
        #[cfg(feature = "storage")]
        FLASH_CHANNEL.send(FlashOperationMessage::OneShotTimeout(ms)).await;
    }

    pub async fn set_tap_interval(&self, ms: u16) {
        self.keymap.set_tap_interval(ms);
        #[cfg(feature = "storage")]
        FLASH_CHANNEL.send(FlashOperationMessage::TapInterval(ms)).await;
    }

    pub async fn set_tap_capslock_interval(&self, ms: u16) {
        self.keymap.set_tap_capslock_interval(ms);
        #[cfg(feature = "storage")]
        FLASH_CHANNEL.send(FlashOperationMessage::TapCapslockInterval(ms)).await;
    }

    pub async fn set_morse_default_profile(&self, profile: MorseProfile) {
        self.keymap.set_morse_default_profile(profile);
        #[cfg(feature = "storage")]
        FLASH_CHANNEL
            .send(FlashOperationMessage::MorseDefaultProfile(profile))
            .await;
    }

    pub async fn set_morse_prior_idle_time(&self, ms: u16) {
        self.keymap.set_morse_prior_idle_time(Duration::from_millis(ms as u64));
        #[cfg(feature = "storage")]
        FLASH_CHANNEL.send(FlashOperationMessage::PriorIdleTime(ms)).await;
    }

    // ── Layout / reset ───────────────────────────────────────────────────

    pub async fn layout_options(&self) -> u32 {
        #[cfg(feature = "storage")]
        {
            crate::storage::read_layout_options().await.unwrap_or(0)
        }
        #[cfg(not(feature = "storage"))]
        {
            0
        }
    }

    pub async fn set_layout_options(&self, opts: u32) {
        // Make the change live immediately; do not wait for the flash worker.
        #[cfg(feature = "universal_symbols")]
        crate::universal_symbols::set_layout_options(opts);
        #[cfg(feature = "storage")]
        FLASH_CHANNEL.send(FlashOperationMessage::LayoutOptions(opts)).await;
        #[cfg(not(feature = "storage"))]
        let _ = opts;
    }

    pub async fn reset_storage(&self) {
        #[cfg(feature = "storage")]
        FLASH_CHANNEL.send(FlashOperationMessage::Reset).await;
    }

    // ── Live state ───────────────────────────────────────────────────────

    pub fn led_indicator(&self) -> LedIndicator {
        crate::keyboard::current_led_indicator()
    }

    pub fn connection_status(&self) -> ConnectionStatus {
        crate::state::current_connection_status()
    }

    #[cfg(feature = "_ble")]
    pub fn battery_status(&self) -> BatteryStatus {
        crate::input_device::battery::current_battery_status()
    }

    #[cfg(all(feature = "split", feature = "_ble"))]
    pub fn peripheral_battery_status(&self, id: usize) -> BatteryStatus {
        crate::split::battery::current_peripheral_battery_status(id)
    }

    pub fn active_layer(&self) -> u8 {
        self.keymap.active_layer()
    }

    pub fn default_layer(&self) -> u8 {
        self.keymap.get_default_layer()
    }

    // ── Matrix state (host_security) ─────────────────────────────────────

    #[cfg(feature = "host_security")]
    pub fn read_matrix_state(&self, target: &mut [u8]) {
        self.keymap.read_matrix_state(target);
    }
}

/// Map a flat keymap index back to `(row, col, layer)`.
///
/// Layout: `index = layer * (rows * cols) + row * cols + col`.
fn position_from_flat_index(index: usize, rows: usize, cols: usize) -> (usize, usize, usize) {
    let layer = index / (cols * rows);
    let layer_offset = index % (cols * rows);
    let row = layer_offset / cols;
    let col = layer_offset % cols;
    (row, col, layer)
}
