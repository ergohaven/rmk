//! Ergohaven product-level user key actions.
//!
//! RMK core treats [`Action::User`] as an application-defined action and only
//! publishes [`ActionEvent`]. Ergohaven firmware registers this processor where
//! the historical UserN mapping should control BLE profiles, transport output,
//! split peer reset, or peripheral battery refresh.

use embassy_time::{Duration, Instant};
use rmk_macro::processor;
use rmk_types::action::Action;
use rmk_types::connection::ConnectionType;

use crate::NUM_BLE_PROFILE;
use crate::ble::profile::BleProfileAction;
use crate::channel::BLE_PROFILE_CHANNEL;
use crate::event::{ActionEvent, KeyboardEvent};
#[cfg(feature = "split")]
use crate::event::{ClearPeerEvent, PeripheralBatteryRefreshEvent, publish_event};

const USER_BT_NEXT: u8 = NUM_BLE_PROFILE as u8;
const USER_BT_PREV: u8 = NUM_BLE_PROFILE as u8 + 1;
const USER_BT_CLEAR: u8 = NUM_BLE_PROFILE as u8 + 2;
const USER_BT_TOGGLE: u8 = NUM_BLE_PROFILE as u8 + 3;
const USER_BT_CLEAR_PEER: u8 = NUM_BLE_PROFILE as u8 + 4;

const K04_USER_BT_PROFILE0: u8 = 19;
const K04_USER_BT_PROFILE1: u8 = 20;
const K04_USER_BT_PROFILE2: u8 = 21;
const K04_USER_BT_PROFILE3: u8 = 22;
const K04_USER_BT_PROFILE4: u8 = 23;
const K04_USER_BT_NEXT: u8 = 24;
const K04_USER_BT_PREV: u8 = 25;
const K04_USER_BT_CLEAR: u8 = 26;
const K04_USER_BT_TOGGLE: u8 = 27;
const K04_USER_BT_OUTPUT: u8 = 37;
const K04_USER_USB_OUTPUT: u8 = 38;
const K04_USER_BATTERY_LEVEL: u8 = 39;
const K04_USER_BT_CLEAR_PEER: u8 = 40;

const CLEAR_PEER_HOLD: Duration = Duration::from_secs(5);

/// Processes Ergohaven's historical UserN assignments.
#[processor(subscribe = [ActionEvent, KeyboardEvent], poll_interval = 50)]
pub struct ErgohavenUserKeys {
    clear_peer_deadline: Option<Instant>,
    clear_peer_key: Option<KeyboardEvent>,
}

impl Default for ErgohavenUserKeys {
    fn default() -> Self {
        Self::new()
    }
}

impl ErgohavenUserKeys {
    pub const fn new() -> Self {
        Self {
            clear_peer_deadline: None,
            clear_peer_key: None,
        }
    }

    async fn on_action_event(&mut self, event: ActionEvent) {
        let Action::User(id) = event.action else {
            return;
        };

        let ble_id = k04_common_ble_id(id).unwrap_or(id);
        if event.keyboard_event.pressed {
            if ble_id == USER_BT_CLEAR_PEER {
                self.arm_clear_peer(event.keyboard_event);
            }
            return;
        }

        match id {
            K04_USER_BT_OUTPUT => {
                set_preferred_connection(ConnectionType::Ble).await;
                return;
            }
            K04_USER_USB_OUTPUT => {
                set_preferred_connection(ConnectionType::Usb).await;
                return;
            }
            K04_USER_BATTERY_LEVEL => {
                #[cfg(feature = "split")]
                publish_event(PeripheralBatteryRefreshEvent);
                return;
            }
            _ => {}
        }

        match ble_id {
            id if id < NUM_BLE_PROFILE as u8 => {
                info!("Switch to profile: {}", id);
                BLE_PROFILE_CHANNEL.send(BleProfileAction::Switch(id)).await;
            }
            USER_BT_NEXT => {
                BLE_PROFILE_CHANNEL.send(BleProfileAction::Next).await;
            }
            USER_BT_PREV => {
                BLE_PROFILE_CHANNEL.send(BleProfileAction::Previous).await;
            }
            USER_BT_CLEAR => {
                BLE_PROFILE_CHANNEL.send(BleProfileAction::ClearBond).await;
            }
            USER_BT_TOGGLE => {
                #[cfg(not(feature = "_no_usb"))]
                crate::state::toggle_preferred().await;
            }
            _ => {}
        }
    }

    async fn on_keyboard_event(&mut self, event: KeyboardEvent) {
        if self.clear_peer_deadline.is_some() && Some(event) != self.clear_peer_key {
            self.clear_peer_deadline = None;
            self.clear_peer_key = None;
        }
    }

    async fn poll(&mut self) {
        let Some(deadline) = self.clear_peer_deadline else {
            return;
        };
        if Instant::now() < deadline {
            return;
        }

        self.clear_peer_deadline = None;
        self.clear_peer_key = None;

        #[cfg(feature = "split")]
        {
            publish_event(ClearPeerEvent);
            info!("Clear peer");
        }
    }

    fn arm_clear_peer(&mut self, event: KeyboardEvent) {
        #[cfg(feature = "split")]
        {
            self.clear_peer_deadline = Some(Instant::now() + CLEAR_PEER_HOLD);
            self.clear_peer_key = Some(event);
        }

        #[cfg(not(feature = "split"))]
        {
            let _ = event;
        }
    }
}

fn k04_common_ble_id(id: u8) -> Option<u8> {
    match id {
        K04_USER_BT_PROFILE0 => Some(0),
        K04_USER_BT_PROFILE1 => Some(1),
        K04_USER_BT_PROFILE2 => Some(2),
        K04_USER_BT_PROFILE3 => Some(3),
        K04_USER_BT_PROFILE4 => Some(4),
        K04_USER_BT_NEXT => Some(USER_BT_NEXT),
        K04_USER_BT_PREV => Some(USER_BT_PREV),
        K04_USER_BT_CLEAR => Some(USER_BT_CLEAR),
        K04_USER_BT_TOGGLE => Some(USER_BT_TOGGLE),
        K04_USER_BT_CLEAR_PEER => Some(USER_BT_CLEAR_PEER),
        _ => None,
    }
}

async fn set_preferred_connection(connection_type: ConnectionType) {
    crate::state::set_preferred_connection_persistent(connection_type).await;
}
