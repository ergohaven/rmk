//! USB-only maintenance for a fixed-role split peripheral. No keymap or settings
//! service is created, and no central/split link is needed to unlock locally.

use core::cell::{Cell, RefCell};

use embassy_sync::pubsub::WaitResult;
use embedded_io_async::{Read, Write};
use postcard::experimental::max_size::MaxSize;
use rmk_types::protocol::rynk::ergohaven::{FirmwareTarget, caps};
use rmk_types::protocol::rynk::{
    Cmd, Deframer, DeviceCapabilities, RYNK_HEADER_SIZE, RynkError, RynkMessage, command, encode_frame, max_wire_size,
};

use super::RYNK_UNLOCK_WINDOW;
use super::ergohaven::{board_definition_chunk, ergohaven_info};
use super::handlers::{serve, system};
use crate::config::{DeviceConfig, ErgohavenConfig};
use crate::event::{KeyboardEvent, SubscribableEvent};
use crate::host::lock::HostLock;
use crate::matrix::MatrixState;

pub(crate) struct PeripheralRynkService {
    device: DeviceConfig<'static>,
    ergohaven: ErgohavenConfig<'static>,
    id: u8,
    rows: u8,
    cols: u8,
    unlock_keys: &'static [(u8, u8)],
    matrix: RefCell<MatrixState>,
    /// Lost key events invalidate held keys and any session authorization.
    key_epoch: Cell<u32>,
}

impl PeripheralRynkService {
    pub(crate) fn new(
        device: DeviceConfig<'static>,
        ergohaven: ErgohavenConfig<'static>,
        id: u8,
        rows: u8,
        cols: u8,
        unlock_keys: &'static [(u8, u8)],
    ) -> Self {
        Self {
            device,
            ergohaven,
            id,
            rows,
            cols,
            unlock_keys,
            matrix: RefCell::new(MatrixState::new(rows as usize, cols as usize)),
            key_epoch: Cell::new(0),
        }
    }

    /// Always drain local scanner events, including while USB/the central is
    /// absent. This subscriber is independent of the split transport task.
    pub(crate) async fn run_keys(&self) {
        let mut keys = KeyboardEvent::subscriber();
        loop {
            match keys.next_message().await {
                WaitResult::Message(event) => self.matrix.borrow_mut().update(&event),
                WaitResult::Lagged(_) => {
                    *self.matrix.borrow_mut() = MatrixState::new(self.rows as usize, self.cols as usize);
                    self.key_epoch.set(self.key_epoch.get().wrapping_add(1));
                }
            }
        }
    }

    async fn dispatch(&self, lock: &HostLock<'_>, msg: &mut RynkMessage<'_>) -> Result<(), RynkError> {
        match msg.header().cmd {
            Cmd::GetVersion => {
                msg.decode_request::<()>()?;
                msg.encode_response(&rmk_types::protocol::rynk::ProtocolVersion::CURRENT)
            }
            Cmd::GetCapabilities => {
                msg.decode_request::<()>()?;
                msg.encode_response(&DeviceCapabilities {
                    num_rows: self.rows,
                    num_cols: self.cols,
                    is_split: true,
                    max_payload_size: rmk_types::protocol::rynk::RYNK_MAX_PAYLOAD_SIZE as u16,
                    ..Default::default()
                })
            }
            Cmd::GetDeviceInfo => {
                msg.decode_request::<()>()?;
                msg.encode_response(&system::device_info(&self.device))
            }
            Cmd::GetErgohavenInfo => {
                msg.decode_request::<()>()?;
                msg.encode_response(&ergohaven_info(
                    &self.ergohaven,
                    caps::FIRMWARE_TARGET | caps::USB_MAINTENANCE,
                ))
            }
            Cmd::GetBoardDefinition => {
                let offset = msg.decode_request::<u32>()?;
                msg.encode_response(&board_definition_chunk(self.ergohaven.board_definition, offset)?)
            }
            Cmd::GetFirmwareTarget => {
                msg.decode_request::<()>()?;
                msg.encode_response(&FirmwareTarget::Peripheral(self.id))
            }
            Cmd::GetLockStatus => serve::<command::GetLockStatus, _>(lock, msg).await,
            Cmd::UnlockPoll => serve::<command::UnlockPoll, _>(lock, msg).await,
            Cmd::Lock => serve::<command::Lock, _>(lock, msg).await,
            Cmd::BootloaderJump => {
                msg.decode_request::<()>()?;
                if !lock.is_unlocked() {
                    return Err(RynkError::Locked);
                }
                // The only write operation exposed here. Runtime flash writes
                // remain the bootloader's responsibility, just as on a central.
                crate::boot::jump_to_bootloader();
                msg.encode_response(&())
            }
            _ => Err(RynkError::UnknownCmd),
        }
    }

    pub(crate) async fn run_session<R: Read, W: Write>(&self, rx: &mut R, tx: &mut W) {
        // USB reset/disconnect drops this gate. No insecure-mode escape hatch.
        let lock = HostLock::new(self.unlock_keys, &self.matrix, false, RYNK_UNLOCK_WINDOW);
        let mut epoch = self.key_epoch.get();
        let mut buf = [0; rmk_types::constants::RYNK_BUFFER_SIZE];
        let mut df = Deframer::new();
        loop {
            while let Some(len) = df.next(&mut buf) {
                if self.key_epoch.get() != epoch {
                    lock.lock();
                    epoch = self.key_epoch.get();
                }
                let parked = df.park_pending(&mut buf);
                let capacity = buf.len() - parked;
                let mut msg = RynkMessage::from_decoded(&mut buf[..capacity], len);
                if !msg.header().cmd.is_topic() {
                    let result = self.dispatch(&lock, &mut msg).await;
                    let written = match result {
                        Ok(()) => tx.write_all(msg.frame()).await,
                        Err(error) => {
                            let error = if parked > 0 && error == RynkError::Internal {
                                RynkError::Busy
                            } else {
                                error
                            };
                            let mut frame = [0; max_wire_size(
                                RYNK_HEADER_SIZE + <Result<(), RynkError> as MaxSize>::POSTCARD_MAX_SIZE,
                            )];
                            let len = encode_frame(&mut frame, msg.header(), &Err::<(), _>(error)).unwrap_or(0);
                            tx.write_all(&frame[..len]).await
                        }
                    };
                    if written.is_err() {
                        return;
                    }
                }
                df.unpark_pending(&mut buf, parked);
            }
            match rx.read(df.tail(&mut buf)).await {
                Ok(0) | Err(_) => return,
                Ok(n) => df.commit(n),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use alloc::collections::VecDeque;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::sync::atomic::Ordering;

    use embedded_io_async::{ErrorKind, ErrorType};
    use rmk_types::protocol::rynk::ergohaven::{ERGOHAVEN_RYNK_REVISION, ErgohavenInfo};
    use rmk_types::protocol::rynk::{DeviceInfo, FirmwareVersion, LayoutChunk, LockStatus};

    use super::super::tests::{ChunkRead, VecWrite, decode_frames, req, req_with};
    use super::*;
    use crate::event::publish_event;
    use crate::test_support::test_block_on as block_on;

    fn service() -> PeripheralRynkService {
        PeripheralRynkService::new(
            DeviceConfig::default(),
            ErgohavenConfig {
                keyboard_uid: &[1, 2, 3, 4, 5, 6, 7, 8],
                board_definition: b"board",
                firmware_version: 0x0000_0301,
                ..Default::default()
            },
            1,
            2,
            2,
            &[(0, 0), (1, 1)],
        )
    }

    fn requests(service: &PeripheralRynkService, bytes: Vec<u8>) -> Vec<(u16, u8, Vec<u8>)> {
        let mut rx = ChunkRead {
            chunks: VecDeque::from([bytes]),
        };
        let mut tx = VecWrite { captured: Vec::new() };
        block_on(service.run_session(&mut rx, &mut tx));
        decode_frames(&tx.captured)
    }

    #[test]
    fn maintenance_reports_component_identity_and_only_maintenance_capabilities() {
        let service = service();
        let commands = [
            Cmd::GetVersion,
            Cmd::GetCapabilities,
            Cmd::GetDeviceInfo,
            Cmd::GetErgohavenInfo,
            Cmd::GetFirmwareTarget,
        ];
        let frames = requests(
            &service,
            commands
                .iter()
                .enumerate()
                .flat_map(|(i, cmd)| req(cmd.raw(), i as u8))
                .collect(),
        );
        assert_eq!(frames.len(), commands.len());
        let caps = postcard::from_bytes::<Result<DeviceCapabilities, RynkError>>(&frames[1].2)
            .unwrap()
            .unwrap();
        assert_eq!((caps.num_rows, caps.num_cols, caps.num_layers), (2, 2, 0));
        assert!(!caps.macros_writable && !caps.storage_enabled && !caps.bulk_transfer_supported);
        let device = postcard::from_bytes::<Result<DeviceInfo, RynkError>>(&frames[2].2)
            .unwrap()
            .unwrap();
        assert_eq!(device.serial_number, service.device.serial_number);
        let info = postcard::from_bytes::<Result<ErgohavenInfo, RynkError>>(&frames[3].2)
            .unwrap()
            .unwrap();
        assert_eq!(
            info.firmware,
            FirmwareVersion {
                major: 0,
                minor: 3,
                patch: 1
            }
        );
        assert_eq!(info.keyboard_uid, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(info.revision, ERGOHAVEN_RYNK_REVISION);
        assert_eq!(info.caps, caps::USB_MAINTENANCE | caps::FIRMWARE_TARGET);
        assert_eq!(
            postcard::from_bytes::<Result<FirmwareTarget, RynkError>>(&frames[4].2).unwrap(),
            Ok(FirmwareTarget::Peripheral(1))
        );
    }

    #[test]
    fn locked_maintenance_reads_the_topology_definition_by_offset() {
        let service = service();
        for (offset, expected) in [(0u32, b"board".as_slice()), (2, b"ard"), (6, b"")] {
            let frames = requests(&service, req_with(Cmd::GetBoardDefinition.raw(), 0, &offset));
            let chunk = postcard::from_bytes::<Result<LayoutChunk, RynkError>>(&frames[0].2)
                .unwrap()
                .unwrap();
            assert_eq!(chunk.total_len, 5);
            assert_eq!(chunk.bytes.as_slice(), expected);
        }
    }

    #[test]
    fn maintenance_rejects_locked_boot_and_all_configuration_commands() {
        let service = service();
        let before = crate::boot::BOOTLOADER_JUMPS.load(Ordering::Relaxed);
        let commands = [
            Cmd::BootloaderJump,
            Cmd::Reboot,
            Cmd::StorageReset,
            Cmd::GetMatrixState,
            Cmd::GetKeyAction,
            Cmd::SetKeyAction,
            Cmd::SetSetting,
        ];
        let frames = requests(
            &service,
            commands
                .iter()
                .enumerate()
                .flat_map(|(i, cmd)| req(cmd.raw(), i as u8))
                .collect(),
        );
        assert_eq!(frames.len(), commands.len());
        for (i, frame) in frames.iter().enumerate() {
            assert_eq!(frame.1, i as u8);
            assert_eq!(
                postcard::from_bytes::<Result<(), RynkError>>(&frame.2).unwrap(),
                Err(if i == 0 {
                    RynkError::Locked
                } else {
                    RynkError::UnknownCmd
                })
            );
        }
        assert_eq!(crate::boot::BOOTLOADER_JUMPS.load(Ordering::Relaxed), before);
    }

    struct ScannerHost {
        steps: VecDeque<(Cmd, Vec<KeyboardEvent>)>,
    }
    impl ErrorType for ScannerHost {
        type Error = ErrorKind;
    }
    impl Read for ScannerHost {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, ErrorKind> {
            let Some((cmd, keys)) = self.steps.pop_front() else {
                return Ok(0);
            };
            for event in keys {
                publish_event(event);
            }
            // Let the real maintenance listener consume scanner publications.
            embassy_futures::yield_now().await;
            let frame = req(cmd.raw(), 0);
            buf[..frame.len()].copy_from_slice(&frame);
            Ok(frame.len())
        }
    }

    #[test]
    fn local_keys_unlock_without_a_central_and_lock_and_usb_reconnect_revoke_access() {
        // The split forwarder stays subscribed while USB maintenance runs.
        let _split_keys = KeyboardEvent::subscriber();
        #[cfg(feature = "_ble")]
        let _wake_keys = crate::ble::sleep::InputActivityWaiter::new();
        let service = service();
        let before = crate::boot::BOOTLOADER_JUMPS.load(Ordering::Relaxed);
        let mut rx = ScannerHost {
            steps: VecDeque::from([
                (Cmd::UnlockPoll, vec![KeyboardEvent::key(0, 0, true)]),
                (Cmd::BootloaderJump, vec![]),
                (Cmd::UnlockPoll, vec![KeyboardEvent::key(1, 1, true)]),
                (Cmd::BootloaderJump, vec![]),
                (Cmd::Lock, vec![]),
                (Cmd::BootloaderJump, vec![]),
                (Cmd::UnlockPoll, vec![]),
            ]),
        };
        let mut tx = VecWrite { captured: Vec::new() };
        block_on(embassy_futures::select::select(
            service.run_keys(),
            service.run_session(&mut rx, &mut tx),
        ));
        let frames = decode_frames(&tx.captured);
        let partial = postcard::from_bytes::<Result<LockStatus, RynkError>>(&frames[0].2)
            .unwrap()
            .unwrap();
        assert!(partial.locked);
        assert_eq!(partial.remaining_keys, 1);
        assert_eq!(partial.key_positions.as_slice(), &[(0, 0), (1, 1)]);
        let unlocked = postcard::from_bytes::<Result<LockStatus, RynkError>>(&frames[2].2)
            .unwrap()
            .unwrap();
        assert!(!unlocked.locked);
        for i in [1, 5] {
            assert_eq!(
                postcard::from_bytes::<Result<(), RynkError>>(&frames[i].2).unwrap(),
                Err(RynkError::Locked)
            );
        }
        assert_eq!(crate::boot::BOOTLOADER_JUMPS.load(Ordering::Relaxed), before + 1);
        // Held keys alone never authorize a fresh USB session; polling is required.
        let frames = requests(&service, req(Cmd::BootloaderJump.raw(), 0));
        assert_eq!(
            postcard::from_bytes::<Result<(), RynkError>>(&frames[0].2).unwrap(),
            Err(RynkError::Locked)
        );
    }

    #[cfg(not(feature = "_no_usb"))]
    #[test]
    fn usb_reset_cancels_an_unlocked_session_without_waiting_for_reader_eof() {
        use crate::usb::{HostSession, PERIPHERAL_USB_RESET};
        struct ResetReader {
            step: usize,
        }
        impl ErrorType for ResetReader {
            type Error = ErrorKind;
        }
        impl Read for ResetReader {
            async fn read(&mut self, buf: &mut [u8]) -> Result<usize, ErrorKind> {
                let cmd = match self.step {
                    0 => {
                        publish_event(KeyboardEvent::key(0, 0, true));
                        publish_event(KeyboardEvent::key(1, 1, true));
                        Cmd::UnlockPoll
                    }
                    1 => {
                        PERIPHERAL_USB_RESET.signal(());
                        Cmd::BootloaderJump
                    }
                    _ => return Ok(0),
                };
                self.step += 1;
                embassy_futures::yield_now().await;
                let frame = req(cmd.raw(), 0);
                buf[..frame.len()].copy_from_slice(&frame);
                Ok(frame.len())
            }
        }
        let service = service();
        let before = crate::boot::BOOTLOADER_JUMPS.load(Ordering::Relaxed);
        let mut rx = ResetReader { step: 0 };
        let mut tx = VecWrite { captured: Vec::new() };
        block_on(embassy_futures::select::select(
            service.run_keys(),
            service.serve(&mut rx, &mut tx),
        ));
        let frames = decode_frames(&tx.captured);
        let status = postcard::from_bytes::<Result<LockStatus, RynkError>>(&frames[0].2)
            .unwrap()
            .unwrap();
        assert!(!status.locked, "the session really unlocked before the reset");
        assert_eq!(crate::boot::BOOTLOADER_JUMPS.load(Ordering::Relaxed), before);
        assert_eq!(frames.len(), 1, "old-session requests must be discarded");
        let frames = requests(&service, req(Cmd::BootloaderJump.raw(), 0));
        assert_eq!(
            postcard::from_bytes::<Result<(), RynkError>>(&frames[0].2).unwrap(),
            Err(RynkError::Locked)
        );
    }

    #[test]
    fn lost_scanner_events_revoke_unlock_and_clear_held_keys() {
        let service = service();
        let before = crate::boot::BOOTLOADER_JUMPS.load(Ordering::Relaxed);
        let mut flood = vec![KeyboardEvent::key(0, 0, false), KeyboardEvent::key(1, 1, false)];
        // Overwrite releases with unrelated events while the listener is parked.
        flood.extend((0..crate::KEYBOARD_EVENT_CHANNEL_SIZE + 1).map(|_| KeyboardEvent::key(0, 1, true)));
        let mut rx = ScannerHost {
            steps: VecDeque::from([
                (
                    Cmd::UnlockPoll,
                    vec![KeyboardEvent::key(0, 0, true), KeyboardEvent::key(1, 1, true)],
                ),
                (Cmd::BootloaderJump, flood),
                (Cmd::UnlockPoll, vec![]),
            ]),
        };
        let mut tx = VecWrite { captured: Vec::new() };
        block_on(embassy_futures::select::select(
            service.run_keys(),
            service.run_session(&mut rx, &mut tx),
        ));
        let frames = decode_frames(&tx.captured);
        assert_eq!(
            postcard::from_bytes::<Result<(), RynkError>>(&frames[1].2).unwrap(),
            Err(RynkError::Locked)
        );
        let status = postcard::from_bytes::<Result<LockStatus, RynkError>>(&frames[2].2)
            .unwrap()
            .unwrap();
        assert!(status.locked);
        assert_eq!(status.remaining_keys, 2);
        assert_eq!(crate::boot::BOOTLOADER_JUMPS.load(Ordering::Relaxed), before);
    }

    #[test]
    fn fragmented_requests_and_unknown_topics_do_not_break_the_next_reply() {
        let service = service();
        let mut bytes = req(0x8001, 0);
        bytes.extend(req(Cmd::GetFirmwareTarget.raw(), 7));
        let mut rx = ChunkRead {
            chunks: bytes.into_iter().map(|b| vec![b]).collect(),
        };
        let mut tx = VecWrite { captured: Vec::new() };
        block_on(service.run_session(&mut rx, &mut tx));
        let frames = decode_frames(&tx.captured);
        assert_eq!(frames.len(), 1);
        assert_eq!((frames[0].0, frames[0].1), (Cmd::GetFirmwareTarget.raw(), 7));
    }
}
