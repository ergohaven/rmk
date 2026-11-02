//! Connection handlers — USB/BLE preferred-transport read, BLE profile management.

use rmk_types::ble::BleState;
#[cfg(feature = "_ble")]
use rmk_types::ble::BleStatus;
use rmk_types::connection::{ConnectionStatus, ConnectionType};
use rmk_types::protocol::rynk::RynkError;
#[cfg(feature = "_ble")]
use rmk_types::protocol::rynk::command::{ClearBleProfile, GetBleStatus, SwitchBleProfile};
use rmk_types::protocol::rynk::command::{GetConnectionStatus, GetConnectionType};

use super::super::RynkService;
use super::Handle;

impl Handle<GetConnectionType> for RynkService<'_> {
    async fn handle(&self, _: ()) -> Result<ConnectionType, RynkError> {
        Ok(self.ctx.preferred_connection())
    }
}

/// `status` as RMK's wire has it. `BleState::Sleeping`, a host link this
/// firmware dropped while idle, is no part of that wire, so it goes out as
/// `Inactive`, which is what an RMK host makes of it.
pub(in crate::host::rynk) fn wire_connection_status(mut status: ConnectionStatus) -> ConnectionStatus {
    if status.ble.state == BleState::Sleeping {
        status.ble.state = BleState::Inactive;
    }
    status
}

/// `Cmd::GetConnectionStatus` — the same payload the `ConnectionChange`
/// topic pushes, so a host can recover a missed push.
impl Handle<GetConnectionStatus> for RynkService<'_> {
    async fn handle(&self, _: ()) -> Result<ConnectionStatus, RynkError> {
        Ok(wire_connection_status(self.ctx.connection_status()))
    }
}

#[cfg(feature = "_ble")]
impl Handle<GetBleStatus> for RynkService<'_> {
    async fn handle(&self, _: ()) -> Result<BleStatus, RynkError> {
        Ok(wire_connection_status(self.ctx.connection_status()).ble)
    }
}

/// `Cmd::SwitchBleProfile` — payload is the slot index. `try_send` is
/// used so a host hammering this Cmd while the previous switch is still
/// running observes the queue-full error rather than blocking the
/// dispatch loop.
#[cfg(feature = "_ble")]
impl Handle<SwitchBleProfile> for RynkService<'_> {
    async fn handle(&self, slot: u8) -> Result<(), RynkError> {
        Self::check_ble_profile_slot(slot)?;
        crate::channel::BLE_PROFILE_CHANNEL
            .try_send(crate::ble::profile::BleProfileAction::Switch(slot))
            .map_err(|_| RynkError::NotReady)
    }
}

/// `Cmd::ClearBleProfile` — wipes the bond at the given slot without
/// requiring a prior switch (uses [`BleProfileAction::ClearSlot`]).
#[cfg(feature = "_ble")]
impl Handle<ClearBleProfile> for RynkService<'_> {
    async fn handle(&self, slot: u8) -> Result<(), RynkError> {
        Self::check_ble_profile_slot(slot)?;
        crate::channel::BLE_PROFILE_CHANNEL
            .try_send(crate::ble::profile::BleProfileAction::ClearSlot(slot))
            .map_err(|_| RynkError::NotReady)
    }
}

#[cfg(feature = "_ble")]
impl RynkService<'_> {
    /// `Invalid` for a BLE profile slot past the configured profile count.
    fn check_ble_profile_slot(slot: u8) -> Result<(), RynkError> {
        if (slot as usize) >= crate::NUM_BLE_PROFILE {
            Err(RynkError::Invalid)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use rmk_types::ble::{BleState, BleStatus};
    use rmk_types::connection::{ConnectionStatus, ConnectionType, UsbState};

    use super::wire_connection_status;

    fn status(state: BleState) -> ConnectionStatus {
        ConnectionStatus {
            usb: UsbState::Disabled,
            ble: BleStatus {
                profile: 2,
                state,
                bonded: true,
            },
            preferred: ConnectionType::Ble,
        }
    }

    #[test]
    fn a_sleeping_host_link_goes_out_as_inactive() {
        assert_eq!(
            wire_connection_status(status(BleState::Sleeping)),
            status(BleState::Inactive)
        );
        for state in [BleState::Advertising, BleState::Connected, BleState::Inactive] {
            assert_eq!(wire_connection_status(status(state)), status(state));
        }
    }
}
