//! Vial over BLE GATT — one per-connection session shared by both transports:
//! the Vial report (id 5) inside the composite HID service, and the vendor
//! `VialGattService` that BlueZ hosts use because they own HOGP services.
//!
//! [`HostGattHandler::run`] owns the per-connection lifecycle: clear the
//! inbound chunk channel, construct 32-byte HID-report Rx/Tx adapters around
//! the GATT plumbing, and call [`VialService::run_session`] once. Replies go
//! back on whichever transport carried the last request (`VialBleSource`).

use core::sync::atomic::{AtomicU8, Ordering};

use embedded_io_async::{ErrorType, Read, Write};
use trouble_host::prelude::*;

use super::HostWriteOutcome;
use crate::ble::ble_server::Server;
use crate::channel::VIAL_BLE_RX_CHANNEL;
use crate::host::transport::HostTransportError;
use crate::host::via::VialService;

/// Per-connection GATT write dispatcher for the two Vial BLE transports.
pub(crate) struct HostGattHandler {
    hid_output_handle: u16,
    hid_input_cccd_handle: u16,
    gatt_output_handle: u16,
    gatt_input_cccd_handle: u16,
}

impl HostGattHandler {
    pub(crate) fn new(server: &Server<'_>) -> Self {
        Self {
            hid_output_handle: server.hid_service.vial_output.handle,
            hid_input_cccd_handle: server
                .hid_service
                .vial_input
                .cccd_handle
                .expect("No CCCD for Vial HID input"),
            gatt_output_handle: server.vial_service.output_data.handle,
            gatt_input_cccd_handle: server
                .vial_service
                .input_data
                .cccd_handle
                .expect("No CCCD for Vial GATT input"),
        }
    }

    pub(crate) async fn handle_write(&mut self, handle: u16, data: &[u8], _encrypted: bool) -> HostWriteOutcome {
        let source = if handle == self.hid_output_handle {
            VialBleSource::Hid
        } else if handle == self.gatt_output_handle {
            VialBleSource::VendorGatt
        } else if handle == self.hid_input_cccd_handle || handle == self.gatt_input_cccd_handle {
            // A subscription alone must not bind the reply transport. OS HOGP
            // drivers may subscribe automatically when restoring a bond.
            return HostWriteOutcome::CccdUpdated;
        } else {
            return HostWriteOutcome::Unhandled;
        };

        if data.len() == 32 {
            debug!("Got Vial packet: {:?}", data);
            let mut message = [0u8; 32];
            message.copy_from_slice(data);
            source.activate();
            VIAL_BLE_RX_CHANNEL.send(message).await;
        } else {
            warn!("Wrong Vial packet data: {:?}", data);
        }
        HostWriteOutcome::Handled
    }

    /// Run one Vial session over `conn`, clearing stale RX chunks first.
    pub(crate) async fn run<'stack, 'server, P: PacketPool>(
        server: &'server Server<'_>,
        conn: &GattConnection<'stack, 'server, P>,
        service: &VialService<'_>,
    ) {
        VIAL_BLE_RX_CHANNEL.clear();
        VialBleSource::Hid.activate();
        let mut rx = VialBleRx;
        let mut tx = VialBleTx {
            hid_input: server.hid_service.vial_input,
            gatt_input: server.vial_service.input_data,
            conn,
        };
        service.run_session(&mut rx, &mut tx).await;
    }
}

/// Which BLE transport carried the host's last Vial request. Set only on a
/// request write, never on a CCCD subscribe, and reset per connection.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
#[repr(u8)]
enum VialBleSource {
    /// Vial report inside the composite HID service.
    Hid,
    /// Vendor `VialGattService`.
    VendorGatt,
}

static ACTIVE_SOURCE: AtomicU8 = AtomicU8::new(VialBleSource::Hid as u8);

impl VialBleSource {
    fn activate(self) {
        ACTIVE_SOURCE.store(self as u8, Ordering::Relaxed);
    }

    fn active() -> Self {
        if ACTIVE_SOURCE.load(Ordering::Relaxed) == Self::VendorGatt as u8 {
            Self::VendorGatt
        } else {
            Self::Hid
        }
    }
}

struct VialBleRx;

impl ErrorType for VialBleRx {
    type Error = HostTransportError;
}

impl Read for VialBleRx {
    /// Vial chunks are always 32 bytes. Callers drive this via
    /// `read_exact(&mut [u8; 32])`; smaller buffers are rejected.
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let chunk = VIAL_BLE_RX_CHANNEL.receive().await;
        if buf.len() < chunk.len() {
            error!(
                "VialBleRx::read called with buf.len() = {} < chunk.len() = {}",
                buf.len(),
                chunk.len()
            );
            return Err(HostTransportError);
        }
        buf[..chunk.len()].copy_from_slice(&chunk);
        Ok(chunk.len())
    }
}

struct VialBleTx<'a, 'b, 'c, P: PacketPool> {
    hid_input: Characteristic<[u8; 32]>,
    gatt_input: Characteristic<[u8; 32]>,
    conn: &'a GattConnection<'b, 'c, P>,
}

impl<P: PacketPool> ErrorType for VialBleTx<'_, '_, '_, P> {
    type Error = HostTransportError;
}

impl<P: PacketPool> Write for VialBleTx<'_, '_, '_, P> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        // Both input characteristics are fixed-size [u8; 32]; expect exactly
        // 32 bytes per write call.
        let arr: &[u8; 32] = buf.try_into().map_err(|_| {
            error!("Vial reply must be exactly 32 bytes, got {}", buf.len());
            HostTransportError
        })?;
        let input = match VialBleSource::active() {
            VialBleSource::Hid => self.hid_input,
            VialBleSource::VendorGatt => self.gatt_input,
        };
        if let Err(e) = input.notify(self.conn, arr, true).await {
            error!("Failed to notify Vial reply: {:?}", e);
            return Err(HostTransportError);
        }
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_block_on as block_on;

    #[test]
    fn classifies_vial_gatt_handles() {
        let server = Server::new_default("rmk").unwrap();
        let mut handler = HostGattHandler::new(&server);

        assert_eq!(
            block_on(handler.handle_write(server.hid_service.vial_output.handle, &[], true)),
            HostWriteOutcome::Handled
        );
        assert_eq!(
            block_on(handler.handle_write(server.vial_service.output_data.handle, &[], true)),
            HostWriteOutcome::Handled
        );
        assert_eq!(
            block_on(handler.handle_write(server.hid_service.vial_input.cccd_handle.unwrap(), &[], true)),
            HostWriteOutcome::CccdUpdated
        );
        assert_eq!(
            block_on(handler.handle_write(server.vial_service.input_data.cccd_handle.unwrap(), &[], true)),
            HostWriteOutcome::CccdUpdated
        );
        // The HID control point belongs to the shared HID service, which
        // `gatt_events_task` handles before asking the host protocol.
        assert_eq!(
            block_on(handler.handle_write(server.hid_service.hid_control_point.handle, &[0], true)),
            HostWriteOutcome::Unhandled
        );
        assert_eq!(
            block_on(handler.handle_write(u16::MAX, &[], true)),
            HostWriteOutcome::Unhandled
        );
    }

    #[test]
    fn replies_follow_the_transport_of_the_last_request() {
        let server = Server::new_default("rmk").unwrap();
        let mut handler = HostGattHandler::new(&server);

        VIAL_BLE_RX_CHANNEL.clear();
        block_on(handler.handle_write(server.vial_service.output_data.handle, &[0; 32], true));
        assert_eq!(VialBleSource::active(), VialBleSource::VendorGatt);

        // A subscribe on the other transport does not move the replies.
        block_on(handler.handle_write(server.hid_service.vial_input.cccd_handle.unwrap(), &[1, 0], true));
        assert_eq!(VialBleSource::active(), VialBleSource::VendorGatt);

        VIAL_BLE_RX_CHANNEL.clear();
        block_on(handler.handle_write(server.hid_service.vial_output.handle, &[0; 32], true));
        assert_eq!(VialBleSource::active(), VialBleSource::Hid);
        VIAL_BLE_RX_CHANNEL.clear();
    }
}
