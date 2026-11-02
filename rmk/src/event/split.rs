//! Split keyboard events

use core::sync::atomic::{AtomicU8, Ordering};

use postcard::experimental::max_size::MaxSize;
use rmk_macro::event;
use serde::{Deserialize, Serialize};

use super::battery::BatteryStatusEvent;

/// Peripheral connected state changed event
#[event(channel_size = crate::PERIPHERAL_CONNECTED_EVENT_CHANNEL_SIZE, pubs = crate::PERIPHERAL_CONNECTED_EVENT_PUB_SIZE, subs = crate::PERIPHERAL_CONNECTED_EVENT_SUB_SIZE)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PeripheralConnectedEvent {
    pub id: usize,
    pub connected: bool,
}

/// Connected to central state changed event
#[event(channel_size = crate::CENTRAL_CONNECTED_EVENT_CHANNEL_SIZE, pubs = crate::CENTRAL_CONNECTED_EVENT_PUB_SIZE, subs = crate::CENTRAL_CONNECTED_EVENT_SUB_SIZE)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CentralConnectedEvent {
    pub connected: bool,
}

/// Current split-link acquisition state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum SplitConnectionState {
    /// The central/peripheral is actively looking for its split peer.
    Searching,
    /// Every required split link is established.
    Connected,
    /// The configured split search window elapsed.
    Idle,
}

const SPLIT_STATE_SEARCHING: u8 = 0;
const SPLIT_STATE_CONNECTED: u8 = 1;
const SPLIT_STATE_IDLE: u8 = 2;

// Split-state events are edge-triggered, while processors subscribe during
// asynchronous startup. Keep a sticky snapshot so a fast post-UF2 reconnect
// cannot leave the LED processor rendering its constructor default forever.
static CURRENT_SPLIT_CONNECTION_STATE: AtomicU8 = AtomicU8::new(SPLIT_STATE_SEARCHING);

/// Store the authoritative split state before its event is published.
pub fn set_current_split_connection_state(state: SplitConnectionState) {
    let raw = match state {
        SplitConnectionState::Searching => SPLIT_STATE_SEARCHING,
        SplitConnectionState::Connected => SPLIT_STATE_CONNECTED,
        SplitConnectionState::Idle => SPLIT_STATE_IDLE,
    };
    CURRENT_SPLIT_CONNECTION_STATE.store(raw, Ordering::Release);
}

/// Read the latest split state even if an edge-triggered event was missed.
pub fn current_split_connection_state() -> SplitConnectionState {
    match CURRENT_SPLIT_CONNECTION_STATE.load(Ordering::Acquire) {
        SPLIT_STATE_CONNECTED => SplitConnectionState::Connected,
        SPLIT_STATE_IDLE => SplitConnectionState::Idle,
        _ => SplitConnectionState::Searching,
    }
}

/// Split-link acquisition state changed event.
#[event(channel_size = crate::SPLIT_CONNECTION_STATE_EVENT_CHANNEL_SIZE, pubs = crate::SPLIT_CONNECTION_STATE_EVENT_PUB_SIZE, subs = crate::SPLIT_CONNECTION_STATE_EVENT_SUB_SIZE)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct SplitConnectionStateEvent(pub SplitConnectionState);

impl_payload_wrapper!(SplitConnectionStateEvent, SplitConnectionState);

/// Peripheral battery status changed event
#[event(channel_size = crate::PERIPHERAL_BATTERY_EVENT_CHANNEL_SIZE, pubs = crate::PERIPHERAL_BATTERY_EVENT_PUB_SIZE, subs = crate::PERIPHERAL_BATTERY_EVENT_SUB_SIZE)]
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, MaxSize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PeripheralBatteryEvent {
    pub id: usize,
    pub state: BatteryStatusEvent,
}

/// Runtime settings packet synced from split central to peripherals.
#[event(channel_size = crate::PERIPHERAL_SETTINGS_EVENT_CHANNEL_SIZE, pubs = crate::PERIPHERAL_SETTINGS_EVENT_PUB_SIZE, subs = crate::PERIPHERAL_SETTINGS_EVENT_SUB_SIZE)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PeripheralSettingsEvent(pub [u8; 27]);

/// Ask the keyboard to re-publish its current settings snapshot.
///
/// Unlike [`PeripheralSettingsEvent`], this never crosses the split link: the
/// central raises it locally whenever a peripheral link comes up, because a
/// peripheral that rebooted on its own starts from hardcoded defaults and the
/// central would otherwise stay silent until the next settings edit.
///
/// Only keyboards that own a settings snapshot subscribe to it, so `subs`
/// defaults to 0 and publishing compiles away unless a board opts in. Tests
/// get one more subscriber and slot, so they can count the publishes.
#[event(channel_size = crate::PERIPHERAL_SETTINGS_REFRESH_EVENT_CHANNEL_SIZE + cfg!(test) as usize, pubs = crate::PERIPHERAL_SETTINGS_REFRESH_EVENT_PUB_SIZE, subs = crate::PERIPHERAL_SETTINGS_REFRESH_EVENT_SUB_SIZE + cfg!(test) as usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PeripheralSettingsRefreshEvent;

/// Request a peripheral battery refresh.
#[cfg(feature = "_ble")]
#[event(channel_size = crate::PERIPHERAL_BATTERY_REFRESH_EVENT_CHANNEL_SIZE, pubs = crate::PERIPHERAL_BATTERY_REFRESH_EVENT_PUB_SIZE, subs = crate::PERIPHERAL_BATTERY_REFRESH_EVENT_SUB_SIZE)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PeripheralBatteryRefreshEvent;

/// Clear BLE peer information event
#[cfg(feature = "_ble")]
#[event(channel_size = crate::CLEAR_PEER_EVENT_CHANNEL_SIZE, pubs = crate::CLEAR_PEER_EVENT_PUB_SIZE, subs = crate::CLEAR_PEER_EVENT_SUB_SIZE)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ClearPeerEvent;
