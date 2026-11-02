//! The abstracted driver layer of the split keyboard.
//!
use core::cell::Cell;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use futures::FutureExt;
use rmk_types::battery::BatteryStatus;
#[cfg(feature = "rynk")]
use rmk_types::protocol::rynk::PeripheralStatus;

use super::{PeripheralMatrixConfig, SplitMessage};
#[cfg(feature = "dfu_split")]
use crate::event::DfuCmdEvent;
#[cfg(feature = "_ble")]
use crate::event::{BatteryStatusEvent, PeripheralBatteryEvent};
use crate::event::{
    KeyboardEvent, KeyboardEventSource, PeripheralConnectedEvent, SubscribableEvent, publish_event, publish_event_async,
};

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum SplitDriverError {
    SerialError,
    EmptyMessage,
    DeserializeError,
    SerializeError,
    BleError(u8),
    Disconnected,
}

/// Split message reader from other split devices
pub(crate) trait SplitReader {
    async fn read(&mut self) -> Result<SplitMessage, SplitDriverError>;
}

/// Split message writer to other split devices
pub(crate) trait SplitWriter {
    async fn write(&mut self, message: &SplitMessage) -> Result<usize, SplitDriverError>;
}

/// Live per-peripheral status. Latched here in the transport-agnostic split
/// layer so host services can read a current snapshot at any time, even when
/// no host session was active when the change happened. Wired peripherals
/// never report a battery, so theirs stays `Unavailable`.
#[derive(Copy, Clone, PartialEq, Eq)]
struct PeripheralSlot {
    connected: bool,
    battery: BatteryStatus,
}

static PERIPHERAL_SLOTS: BlockingMutex<crate::RawMutex, Cell<[PeripheralSlot; crate::SPLIT_PERIPHERALS_NUM]>> =
    BlockingMutex::new(Cell::new(
        [PeripheralSlot {
            connected: false,
            battery: BatteryStatus::Unavailable,
        }; crate::SPLIT_PERIPHERALS_NUM],
    ));

/// Read-modify-write peripheral `id`'s slot. Returns `false` when `id` is out
/// of range or the slot didn't change, so callers skip publishing.
fn update_slot(id: usize, f: impl FnOnce(&mut PeripheralSlot)) -> bool {
    PERIPHERAL_SLOTS.lock(|slots| {
        let mut all = slots.get();
        let Some(slot) = all.get_mut(id) else {
            return false;
        };
        let prev = *slot;
        f(slot);
        if *slot == prev {
            return false;
        }
        slots.set(all);
        true
    })
}

/// Latch peripheral `id`'s connected state and broadcast the change.
pub(crate) fn set_peripheral_connected(id: usize, connected: bool) {
    if update_slot(id, |s| s.connected = connected) {
        publish_event(PeripheralConnectedEvent { id, connected });
    }
}

/// Latch peripheral `id`'s battery status and broadcast the change.
#[cfg(feature = "_ble")]
pub(crate) fn set_peripheral_battery(id: usize, battery: BatteryStatus) {
    if update_slot(id, |s| s.battery = battery) {
        publish_event(PeripheralBatteryEvent {
            id,
            state: BatteryStatusEvent(battery),
        });
    }
}

/// Latest battery status reported by peripheral `id`.
#[cfg(feature = "_ble")]
pub(crate) fn current_peripheral_battery_status(id: usize) -> Option<BatteryStatus> {
    PERIPHERAL_SLOTS.lock(|slots| slots.get().get(id).map(|slot| slot.battery))
}

/// Latest snapshot for peripheral `id`, or `None` when `id` is out of range.
#[cfg(feature = "rynk")]
pub(crate) fn current_peripheral_status(id: usize) -> Option<PeripheralStatus> {
    PERIPHERAL_SLOTS.lock(|slots| {
        slots.get().get(id).map(|s| PeripheralStatus {
            connected: s.connected,
            battery: s.battery,
        })
    })
}

#[cfg(all(test, feature = "_ble"))]
mod tests {
    use rmk_types::battery::ChargeState;

    use super::{current_peripheral_battery_status, set_peripheral_battery};

    #[test]
    fn caches_latest_peripheral_battery_status() {
        let status = rmk_types::battery::BatteryStatus::Available {
            charge_state: ChargeState::Discharging,
            level: Some(73),
        };

        set_peripheral_battery(0, status);

        assert_eq!(current_peripheral_battery_status(0), Some(status));
        assert_eq!(current_peripheral_battery_status(crate::SPLIT_PERIPHERALS_NUM), None);
    }
}

/// PeripheralManager runs on the central and manages communication with one split peripheral.
/// It publishes input events from that peripheral and sends the central's state changes back.
pub(crate) struct PeripheralManager<T: SplitReader + SplitWriter> {
    /// Receiver
    pub(crate) transceiver: T,
    /// Peripheral id
    pub(crate) id: usize,
    /// This peripheral's matrix size and placement in the central's keymap
    matrix_config: PeripheralMatrixConfig,
    #[cfg(feature = "dfu_split")]
    pub(crate) passthrough_crc: crate::crc32::Crc32,
    /// Whether to skip hash comparison and always flash firmware.
    #[cfg(feature = "dfu_split")]
    pub(crate) policy: crate::split::dfu::UpdatePolicy,
    /// Set after a chunk fails all retries — aborts subsequent writes.
    #[cfg(feature = "dfu_split")]
    pub(crate) dfu_aborted: bool,
    /// The layer last sent. A wired session spans the whole uptime of the
    /// central, so this is the current layer; a BLE session starts afresh on
    /// every connection and cannot tell.
    #[cfg(not(feature = "_ble"))]
    layer: u8,
}

impl<T: SplitReader + SplitWriter> PeripheralManager<T> {
    pub(crate) fn new(
        transceiver: T,
        id: usize,
        matrix_config: PeripheralMatrixConfig,
        #[cfg(feature = "dfu_split")] policy: crate::split::dfu::UpdatePolicy,
    ) -> Self {
        Self {
            transceiver,
            matrix_config,
            id,
            #[cfg(feature = "dfu_split")]
            passthrough_crc: crate::crc32::Crc32::new(),
            #[cfg(feature = "dfu_split")]
            policy,
            #[cfg(feature = "dfu_split")]
            dfu_aborted: false,
            #[cfg(not(feature = "_ble"))]
            layer: 0,
        }
    }

    /// Send a message to the peripheral, returning Err on disconnect.
    pub(crate) async fn send(&mut self, msg: &SplitMessage) -> Result<(), ()> {
        debug!("Sending message to peripheral {}: {:?}", self.id, msg);
        #[cfg(not(feature = "_ble"))]
        if let SplitMessage::Layer(layer) = *msg {
            self.layer = layer;
        }
        match self.transceiver.write(msg).await {
            Ok(_) => Ok(()),
            Err(SplitDriverError::Disconnected) => Err(()),
            Err(e) => {
                error!("SplitDriver write error: {:?}", e);
                Err(())
            }
        }
    }

    /// Run the manager.
    ///
    /// The manager receives from the peripheral and publishes input events.
    /// It also syncs the central's `ConnectionStatus` to the peripheral on every
    /// change as an informational signal
    pub(crate) async fn run(mut self) {
        use crate::event::EventSubscriber;

        let mut indicator_sub = crate::event::LedIndicatorEvent::subscriber();
        let mut layer_sub = crate::event::LayerChangeEvent::subscriber();
        let mut settings_sub = crate::event::PeripheralSettingsEvent::subscriber();
        // Subscribe before the initial send so any change racing past the
        // snapshot is still delivered to us.
        let mut connection_sub = crate::event::ConnectionStatusChangeEvent::subscriber();
        #[cfg(feature = "_ble")]
        let mut advertising_mode_sub = crate::event::BleAdvertisingModeEvent::subscriber();
        #[cfg(feature = "_ble")]
        let mut clear_peer_sub = crate::event::ClearPeerEvent::subscriber();
        #[cfg(feature = "_ble")]
        let mut battery_refresh_sub = crate::event::PeripheralBatteryRefreshEvent::subscriber();
        #[cfg(feature = "display")]
        let mut wpm_sub = crate::event::WpmUpdateEvent::subscriber();
        #[cfg(feature = "display")]
        let mut modifier_sub = crate::event::ModifierEvent::subscriber();
        let mut sleep_sub = crate::event::SleepStateEvent::subscriber();
        #[cfg(feature = "dfu_split")]
        let mut dfu_sub = DfuCmdEvent::subscriber();

        // After `settings_sub` exists, so the settings snapshot lands in our
        // subscriber instead of being dropped.
        if self.sync_peripheral().await.is_err() {
            return;
        }

        #[cfg(feature = "dfu_split")]
        self.check_firmware_update().await;

        loop {
            // Use select_biased_with_feature to handle feature-gated subscriber arms
            let next_event_to_peri = async {
                crate::select_biased_with_feature! {
                    e = indicator_sub.next_event().fuse() => SplitMessage::KeyboardIndicator(e.0.into_bits()),
                    e = layer_sub.next_event().fuse() => SplitMessage::Layer(e.0),
                    e = settings_sub.next_event().fuse() => SplitMessage::PeripheralSettings(e.0),
                    e = connection_sub.next_event().fuse() => SplitMessage::ConnectionStatus(e.0),
                    with_feature("_ble"): e = advertising_mode_sub.next_event().fuse() => SplitMessage::BleAdvertisingMode(e.0),
                    with_feature("_ble"): _ = clear_peer_sub.next_event().fuse() => {
                        #[cfg(feature = "storage")]
                        {
                            use {crate::split::ble::PeerAddress, crate::storage::{StorageItem, store_unchecked}};
                            store_unchecked(StorageItem::PeerAddress(PeerAddress::new(self.id as u8, false, [0; 6]))).await;
                        }
                        SplitMessage::ClearPeer
                    },
                    with_feature("_ble"): _ = battery_refresh_sub.next_event().fuse() => {
                        SplitMessage::BatteryRefresh
                    },
                    e = sleep_sub.next_event().fuse() => SplitMessage::SleepState(e.0),
                    with_feature("display"): e = wpm_sub.next_event().fuse() => SplitMessage::Wpm(e.0),
                    with_feature("display"): e = modifier_sub.next_event().fuse() => SplitMessage::Modifier(e.modifier.into_bits()),
                }
            };

            #[cfg(feature = "dfu_split")]
            let event_or_signal = select(next_event_to_peri, dfu_sub.next_message_pure()).fuse();
            #[cfg(not(feature = "dfu_split"))]
            let event_or_signal = next_event_to_peri;

            match select(self.transceiver.read(), event_or_signal).await {
                Either::First(read_result) => match read_result {
                    #[cfg(feature = "dfu_split")]
                    Ok(SplitMessage::FirmwareHashResponse(hash)) => {
                        self.handle_proactive_hash(hash).await;
                    }
                    // A wired peripheral announces itself after rebooting on
                    // its own, while this session kept running.
                    Ok(SplitMessage::ProductId(_)) => {
                        if self.sync_peripheral().await.is_err() {
                            return;
                        }
                    }
                    Ok(split_message) => self.process_peripheral_message(split_message).await,
                    Err(e) => error!("Peripheral message read error: {:?}", e),
                },
                #[cfg(feature = "dfu_split")]
                Either::Second(Either::First(msg)) => {
                    if self.send(&msg).await.is_err() {
                        return;
                    }
                }
                #[cfg(feature = "dfu_split")]
                Either::Second(Either::Second(cmd_event)) => {
                    self.handle_dfu_event(cmd_event).await;
                }
                #[cfg(not(feature = "dfu_split"))]
                Either::Second(msg) => {
                    if self.send(&msg).await.is_err() {
                        return;
                    }
                }
            }
        }
    }

    /// Send the central's state to the peripheral.
    ///
    /// A peripheral that rebooted on its own comes up on hardcoded defaults,
    /// and no settings edit may ever follow to correct it, so this also asks
    /// the keyboard to re-publish its settings snapshot.
    async fn sync_peripheral(&mut self) -> Result<(), ()> {
        publish_event(crate::event::PeripheralSettingsRefreshEvent);

        for message in [
            SplitMessage::ConnectionStatus(crate::state::current_connection_status()),
            SplitMessage::KeyboardIndicator(crate::keyboard::current_led_indicator().into_bits()),
            SplitMessage::SleepState(crate::state::current_sleep_state()),
            #[cfg(not(feature = "_ble"))]
            SplitMessage::Layer(self.layer),
            #[cfg(feature = "_ble")]
            SplitMessage::BleAdvertisingMode(crate::state::current_ble_advertising_mode()),
        ] {
            self.send(&message).await?;
        }
        Ok(())
    }

    /// Process a single message from the peripheral.
    async fn process_peripheral_message(&self, split_message: SplitMessage) {
        trace!("Got message from peripheral: {:?}", split_message);
        match split_message {
            SplitMessage::Key(e) => match e.source {
                KeyboardEventSource::Key(key_pos) => {
                    // Verify the row/col
                    if key_pos.row >= self.matrix_config.rows || key_pos.col >= self.matrix_config.cols {
                        error!("Invalid peripheral row/col: {} {}", key_pos.row, key_pos.col);
                        return;
                    }
                    publish_event_async(KeyboardEvent::key(
                        key_pos.row + self.matrix_config.row_offset,
                        key_pos.col + self.matrix_config.col_offset,
                        e.pressed,
                    ))
                    .await;
                }
                _ => publish_event_async(e).await,
            },
            // Non-key events are drop-on-full to keep the split read loop responsive.
            SplitMessage::Pointing(e) => publish_event(e),
            #[cfg(feature = "_ble")]
            SplitMessage::BatteryStatus(state) => set_peripheral_battery(self.id, state.0),
            #[cfg(feature = "dfu_split")]
            SplitMessage::FirmwareHashResponse(hash) => {
                info!("dfu_split: stale hash response ({:#x}) in event loop", hash);
            }
            #[cfg(feature = "dfu_split")]
            SplitMessage::FirmwareChunkAck { offset, crc: _ } => {
                info!("dfu_split: stale chunk ack (offset {}) in event loop, ignoring", offset);
            }
            #[cfg(feature = "dfu_split")]
            SplitMessage::FirmwareUpdateConfirm => {
                info!("dfu_split: stale update confirm in event loop, ignoring");
            }
            _ => warn!("{:?} should not come from peripheral", split_message),
        }
    }
}

#[cfg(all(test, feature = "_ble"))]
mod sleep_sync_tests {
    use core::cell::RefCell;

    use embassy_futures::select::select;
    use embassy_time::Timer;

    use super::*;
    use crate::test_support::test_block_on as block_on;

    struct RecordingTransceiver<'a> {
        sent: &'a RefCell<heapless::Vec<SplitMessage, 8>>,
    }

    impl SplitReader for RecordingTransceiver<'_> {
        async fn read(&mut self) -> Result<SplitMessage, SplitDriverError> {
            core::future::pending().await
        }
    }

    impl SplitWriter for RecordingTransceiver<'_> {
        async fn write(&mut self, message: &SplitMessage) -> Result<usize, SplitDriverError> {
            let _ = self.sent.borrow_mut().push(*message);
            Ok(0)
        }
    }

    fn startup_sleep_states(sleeping: bool) -> heapless::Vec<bool, 8> {
        crate::ble::sleep::SLEEPING_STATE.store(sleeping, core::sync::atomic::Ordering::Release);
        let sent = RefCell::new(heapless::Vec::new());
        let manager = PeripheralManager::new(
            RecordingTransceiver { sent: &sent },
            0,
            PeripheralMatrixConfig {
                rows: 1,
                cols: 1,
                row_offset: 0,
                col_offset: 0,
            },
            #[cfg(feature = "dfu_split")]
            crate::split::dfu::UpdatePolicy::MatchHash,
        );
        block_on(async {
            select(manager.run(), Timer::after_millis(10)).await;
        });
        sent.borrow()
            .iter()
            .filter_map(|message| match message {
                SplitMessage::SleepState(sleeping) => Some(*sleeping),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn connecting_peripheral_learns_the_keyboard_sleeps() {
        assert_eq!(startup_sleep_states(true), [true]);
    }

    #[test]
    fn connecting_peripheral_learns_the_keyboard_is_awake() {
        assert_eq!(startup_sleep_states(false), [false]);
    }
}

#[cfg(all(test, not(feature = "dfu_split")))]
mod resync_tests {
    use core::cell::RefCell;

    use embassy_futures::select::select;
    use embassy_time::Timer;

    use super::*;
    use crate::test_support::test_block_on as block_on;

    /// A wired peripheral that announces itself once, after the session started.
    struct RebootedPeripheral<'a> {
        announced: bool,
        sent: &'a RefCell<heapless::Vec<SplitMessage, 16>>,
    }

    impl SplitReader for RebootedPeripheral<'_> {
        async fn read(&mut self) -> Result<SplitMessage, SplitDriverError> {
            if !self.announced {
                self.announced = true;
                return Ok(SplitMessage::ProductId(crate::SPLIT_PRODUCT_ID));
            }
            core::future::pending().await
        }
    }

    impl SplitWriter for RebootedPeripheral<'_> {
        async fn write(&mut self, message: &SplitMessage) -> Result<usize, SplitDriverError> {
            let _ = self.sent.borrow_mut().push(*message);
            Ok(0)
        }
    }

    /// Everything the central sends in a session where it first sends
    /// `earlier` and the peripheral then announces a reboot.
    fn session_with_reboot(earlier: &[SplitMessage]) -> heapless::Vec<SplitMessage, 16> {
        let sent = RefCell::new(heapless::Vec::new());
        let mut manager = PeripheralManager::new(
            RebootedPeripheral {
                announced: false,
                sent: &sent,
            },
            0,
            PeripheralMatrixConfig {
                rows: 1,
                cols: 1,
                row_offset: 0,
                col_offset: 0,
            },
        );
        block_on(async {
            for message in earlier {
                manager.send(message).await.unwrap();
            }
            select(manager.run(), Timer::after_millis(10)).await;
        });
        sent.into_inner()
    }

    #[test]
    fn rebooted_peripheral_gets_the_central_state_again() {
        let sent = session_with_reboot(&[]);
        let count = |pick: fn(&SplitMessage) -> bool| sent.iter().filter(|m| pick(m)).count();
        assert_eq!(count(|m| matches!(m, SplitMessage::ConnectionStatus(_))), 2);
        assert_eq!(count(|m| matches!(m, SplitMessage::KeyboardIndicator(_))), 2);
        assert_eq!(count(|m| matches!(m, SplitMessage::SleepState(_))), 2);
    }

    /// The board re-sends its settings snapshot on this request, once when the
    /// session starts and again when the peripheral announces a reboot.
    #[test]
    fn rebooted_peripheral_triggers_a_settings_refresh() {
        let mut refreshes = crate::event::PeripheralSettingsRefreshEvent::subscriber();
        session_with_reboot(&[]);
        assert_eq!(core::iter::from_fn(|| refreshes.try_next_message_pure()).count(), 2);
    }

    /// A wired session knows the current layer and sends it again. A BLE
    /// session only knows the layers it sent itself, so it leaves the layer
    /// to the next change.
    #[test]
    fn rebooted_peripheral_gets_the_current_layer_on_a_wired_link() {
        let sent = session_with_reboot(&[SplitMessage::Layer(3)]);
        let layers: heapless::Vec<u8, 16> = sent
            .iter()
            .filter_map(|message| match message {
                SplitMessage::Layer(layer) => Some(*layer),
                _ => None,
            })
            .collect();
        let expected: &[u8] = if cfg!(feature = "_ble") { &[3] } else { &[3, 3, 3] };
        assert_eq!(layers, expected);
    }
}
