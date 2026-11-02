//! BLE status types and the limits an identity string has to clear.

use postcard::experimental::max_size::MaxSize;
use serde::{Deserialize, Serialize};

/// Longest name a legacy advertisement can carry: flags (3), the battery + HID service
/// UUIDs (6) and the appearance (4) claim the rest of its 31 bytes.
pub const BLE_ADV_NAME_MAX_LEN: usize = 16;

/// BLE state (what the BLE subsystem is currently doing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "wasm", derive(tsify::Tsify))]
#[cfg_attr(feature = "wasm", tsify(into_wasm_abi, from_wasm_abi))]
pub enum BleState {
    /// The BLE is advertising.
    Advertising,
    /// The BLE is connected.
    Connected,
    /// The BLE is not in use (USB mode or sleep mode, default).
    Inactive,
    /// The host link was intentionally disconnected after an idle timeout.
    /// Input wakes advertising, and reports queue until reconnection.
    Sleeping,
}

/// Unified BLE status: active profile, connection state, and whether that profile currently has bond information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "wasm", derive(tsify::Tsify))]
#[cfg_attr(feature = "wasm", tsify(into_wasm_abi, from_wasm_abi))]
pub struct BleStatus {
    pub profile: u8,
    pub state: BleState,
    /// Whether the active BLE profile currently has bond information, independent of connection state.
    pub bonded: bool,
}

impl Default for BleStatus {
    fn default() -> Self {
        Self {
            profile: 0,
            state: BleState::Inactive,
            bonded: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BleState, BleStatus};

    #[test]
    fn default_ble_status_is_profile_zero_and_inactive() {
        assert_eq!(
            BleStatus::default(),
            BleStatus {
                profile: 0,
                state: BleState::Inactive,
                bonded: false,
            }
        );
    }

    #[test]
    fn ble_status_variants_are_copy_and_comparable() {
        let advertising = BleStatus {
            profile: 0,
            state: BleState::Advertising,
            bonded: false,
        };
        let connected = BleStatus {
            profile: 2,
            state: BleState::Connected,
            bonded: true,
        };
        let inactive = BleStatus::default();
        let sleeping = BleStatus {
            profile: 2,
            state: BleState::Sleeping,
            bonded: true,
        };

        assert_ne!(advertising, connected);
        assert_ne!(connected, inactive);
        assert_ne!(sleeping, inactive);
        assert_eq!(
            inactive,
            BleStatus {
                profile: 0,
                state: BleState::Inactive,
                bonded: false,
            }
        );
    }

    #[test]
    fn sleeping_variant_preserves_existing_wire_discriminants() {
        let mut buffer = [0u8; 1];
        assert_eq!(postcard::to_slice(&BleState::Advertising, &mut buffer).unwrap(), &[0]);
        assert_eq!(postcard::to_slice(&BleState::Connected, &mut buffer).unwrap(), &[1]);
        assert_eq!(postcard::to_slice(&BleState::Inactive, &mut buffer).unwrap(), &[2]);
        assert_eq!(postcard::to_slice(&BleState::Sleeping, &mut buffer).unwrap(), &[3]);
    }
}
