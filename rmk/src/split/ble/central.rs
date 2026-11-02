use core::cell::Cell;
use core::sync::atomic::{AtomicU32, Ordering};

#[cfg(feature = "subrating")]
use bt_hci::cmd::le::LeSubrateRequest;
use bt_hci::cmd::le::{LeReadLocalSupportedFeatures, LeSetPhy, LeSetScanParams};
use bt_hci::controller::{ControllerCmdAsync, ControllerCmdSync};
#[cfg(all(not(feature = "subrating"), not(feature = "host_first_split_wake")))]
use embassy_futures::select::select4;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use trouble_host::prelude::*;

use super::GattSplitMessage;
use crate::SPLIT_PAIRING_TIMEOUT_SECONDS;
use crate::ble::adv::Adv;
use crate::ble::scan::{SPLIT_CENTRAL_SCAN_WINDOW, scan_config, start_scan};
use crate::ble::sleep::report_activity;
#[cfg(all(not(feature = "subrating"), feature = "host_first_split_wake"))]
use crate::ble::{HostWakeOrderGate, host_wake_order_gate, wait_for_host_wake_order_gate};
use crate::ble::{update_ble_phy, update_conn_params, wait_for_stack_started};
use crate::event::{
    EventSubscriber, SleepStateEvent, SplitConnectionState, SplitConnectionStateEvent, SubscribableEvent,
    publish_event, set_current_split_connection_state,
};
use crate::split::ble::PeerAddress;
use crate::split::driver::{PeripheralManager, SplitDriverError, SplitReader, SplitWriter, set_peripheral_connected};
use crate::split::{PeripheralMatrixConfig, SPLIT_MESSAGE_MAX_SIZE, SplitMessage};
use crate::storage::{StorageItem, StorageKey, StorageValue, read, store_unchecked};

static PERIPHERAL_FOUND: Signal<crate::RawMutex, (u8, BdAddr)> = Signal::new();

/// One peripheral link's lifecycle, owned by [`scan_and_connect_peripherals`].
enum SlotState {
    /// Unknown address: discover the peripheral by scanning.
    NoAddr,
    /// Known address without a live session: connect the peripheral.
    Disconnected([u8; 6]),
    /// The link is up and handed to the slot's session task.
    Connected([u8; 6]),
}

// The split service and its two characteristics, declared by `#[gatt_service]`
// in `split::ble::peripheral` and discovered by UUID here.
const SPLIT_SERVICE_UUID: u128 = 0x4dd5fbaa_18e5_4b07_bf0a_353698659946;
const MESSAGE_TO_CENTRAL_UUID: u128 = 0x0e6313e3_bd0b_45c2_8d2e_37a2e8128bc3;
const MESSAGE_TO_PERIPHERAL_UUID: u128 = 0x4b3514fb_cae4_4d38_a097_3a2a3d1c3b9c;
#[cfg(feature = "custom_message")]
const CUSTOM_TO_CENTRAL_UUID: u128 = 0x5f2a7c14_9b3e_4a51_8d76_2c1e4b8a6f03;
#[cfg(feature = "custom_message")]
const CUSTOM_TO_PERIPHERAL_UUID: u128 = 0x5f2a7c15_9b3e_4a51_8d76_2c1e4b8a6f03;

// Company id of the product-tagged advertisement sent by halves running the
// previous Ergohaven firmware. Kept so a central updated before its halves can
// still discover them.
const LEGACY_SPLIT_COMPANY_ID: u16 = 0xe118;
const VALIDATED_PEER_FAILURE_LIMIT: u8 = 3;

// Scanned slots whose address is not persisted until the GATT product-id
// handshake proves the peer belongs to this keyboard model.
static UNCOMMITTED_PEER_CANDIDATES: BlockingMutex<crate::RawMutex, Cell<u32>> = BlockingMutex::new(Cell::new(0));
// Slots whose session decided to forget the peer; drained by the scan loop.
static FORGOTTEN_PEERS: BlockingMutex<crate::RawMutex, Cell<u32>> = BlockingMutex::new(Cell::new(0));
static CONNECTED_PERIPHERALS: BlockingMutex<crate::RawMutex, Cell<u32>> = BlockingMutex::new(Cell::new(0));
static PERIPHERAL_CONNECTION_CHANGED: Signal<crate::RawMutex, ()> = Signal::new();
static SPLIT_WINDOW_RESTART: Signal<crate::RawMutex, u32> = Signal::new();
static SPLIT_WINDOW_DONE: Signal<crate::RawMutex, u32> = Signal::new();
static SPLIT_WINDOW_GENERATION: BlockingMutex<crate::RawMutex, Cell<u32>> = BlockingMutex::new(Cell::new(0));
// Links configured with the pointing profile, one bit per peripheral.
static POINTING_LINKS: BlockingMutex<crate::RawMutex, Cell<u32>> = BlockingMutex::new(Cell::new(0));
static LINK_PROFILE_CHANGED: [Signal<crate::RawMutex, ()>; u32::BITS as usize] =
    [const { Signal::new() }; u32::BITS as usize];

static LAST_POINTING_ACTIVITY_MS: AtomicU32 = AtomicU32::new(0);
// PMW3610 may emit background +/-1 reports while settling. Velvet's
// auto-mouse layer uses the same threshold so UI work does not remain deferred
// after meaningful cursor movement has stopped.
const POINTING_ACTIVITY_THRESHOLD: u16 = 2;

// Per-link copy of the last relative motion, 0 until the first report. A
// pointing link keeps its 7.5 ms cadence only this long after the last motion,
// then relaxes to the keyboard cadence: the peripheral attends every
// connection event, so an idle pointing link costs twice the radio time of a
// key-only link. Restoring costs one connection parameter update, so a short
// reading pause must not flap the link.
static LINK_POINTING_ACTIVITY_MS: [AtomicU32; u32::BITS as usize] = [const { AtomicU32::new(0) }; u32::BITS as usize];
static LINK_POINTING_WAKE: [Signal<crate::RawMutex, ()>; u32::BITS as usize] =
    [const { Signal::new() }; u32::BITS as usize];
// A blocking mutex rather than an atomic bitset: ARMv6-M targets (rp2040
// split examples) have no atomic read-modify-write.
static RELAXED_POINTING_LINKS: BlockingMutex<crate::RawMutex, Cell<u32>> = BlockingMutex::new(Cell::new(0));
#[cfg(any(test, all(not(feature = "subrating"), not(feature = "host_first_split_wake"))))]
const POINTING_LINK_HOLD: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailedPeerAction {
    Retain { consecutive_failures: u8 },
    Forget,
}

#[derive(Default)]
struct PeerRetryState {
    validated_failures: u8,
}

impl PeerRetryState {
    fn on_failed_attempt(&mut self, uncommitted: bool) -> FailedPeerAction {
        if uncommitted {
            self.validated_failures = 0;
            return FailedPeerAction::Forget;
        }

        self.validated_failures = self.validated_failures.saturating_add(1);
        if self.validated_failures >= VALIDATED_PEER_FAILURE_LIMIT {
            self.validated_failures = 0;
            FailedPeerAction::Forget
        } else {
            FailedPeerAction::Retain {
                consecutive_failures: self.validated_failures,
            }
        }
    }

    fn on_validated(&mut self) {
        self.validated_failures = 0;
    }
}

/// Active connection cadence of one split link.
///
/// Every link starts with the key-only 15 ms profile. A peripheral that owns a
/// pointing device is switched to the low-latency profile with
/// [`set_split_link_profile`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitLinkProfile {
    Keyboard,
    Pointing,
}

/// Set the active BLE cadence of one split peripheral at runtime.
///
/// This lets a hot-configurable board switch between a key-only link and a
/// 125 Hz pointing link without declaring a second hardware device in TOML.
pub fn set_split_link_profile(peripheral_id: usize, profile: SplitLinkProfile) -> bool {
    let Some(changed) = LINK_PROFILE_CHANGED.get(peripheral_id) else {
        return false;
    };
    let bit = bit_for_peri(peripheral_id);
    let was_pointing = POINTING_LINKS.lock(|links| {
        let current = links.get();
        links.set(match profile {
            SplitLinkProfile::Keyboard => current & !bit,
            SplitLinkProfile::Pointing => current | bit,
        });
        current & bit != 0
    });
    if was_pointing != (profile == SplitLinkProfile::Pointing) {
        changed.signal(());
    }
    true
}

fn split_link_profile(peripheral_id: usize) -> SplitLinkProfile {
    if POINTING_LINKS.lock(Cell::get) & bit_for_peri(peripheral_id) != 0 {
        SplitLinkProfile::Pointing
    } else {
        SplitLinkProfile::Keyboard
    }
}

#[cfg(any(test, all(not(feature = "subrating"), not(feature = "host_first_split_wake"))))]
fn pointing_link_hold_remaining(peripheral_id: usize, now_ms: u32) -> Duration {
    let last_ms = LINK_POINTING_ACTIVITY_MS[peripheral_id.min(31)].load(Ordering::Acquire);
    quiet_period_remaining(now_ms, last_ms, POINTING_LINK_HOLD)
}

/// The cadence a link should run right now: its configured profile, with a
/// pointing link relaxed to the keyboard cadence once its motion hold expires.
#[cfg(any(test, all(not(feature = "subrating"), not(feature = "host_first_split_wake"))))]
fn target_split_link_profile(peripheral_id: usize, now_ms: u32) -> SplitLinkProfile {
    match split_link_profile(peripheral_id) {
        SplitLinkProfile::Pointing if pointing_link_hold_remaining(peripheral_id, now_ms) > Duration::MIN => {
            SplitLinkProfile::Pointing
        }
        _ => SplitLinkProfile::Keyboard,
    }
}

/// Record that `applied` is now the live cadence of a link. A relaxed pointing
/// link is restored by the next motion report instead of a timer.
#[cfg(any(test, all(not(feature = "subrating"), not(feature = "host_first_split_wake"))))]
fn set_applied_split_link_profile(peripheral_id: usize, applied: SplitLinkProfile) {
    let bit = bit_for_peri(peripheral_id);
    let relaxed =
        applied == SplitLinkProfile::Keyboard && split_link_profile(peripheral_id) == SplitLinkProfile::Pointing;
    RELAXED_POINTING_LINKS.lock(|links| {
        let current = links.get();
        links.set(if relaxed { current | bit } else { current & !bit });
    });
}

fn note_link_pointing_activity(peripheral_id: usize, now_ms: u32) {
    let index = peripheral_id.min(31);
    LINK_POINTING_ACTIVITY_MS[index].store(now_ms.max(1), Ordering::Release);
    if RELAXED_POINTING_LINKS.lock(Cell::get) & bit_for_peri(peripheral_id) != 0 {
        LINK_POINTING_WAKE[index].signal(());
    }
}

fn bit_for_peri(peri_id: usize) -> u32 {
    1u32 << peri_id.min(31)
}

fn take_peri_bit(bits: &BlockingMutex<crate::RawMutex, Cell<u32>>, peri_id: usize) -> bool {
    let bit = bit_for_peri(peri_id);
    bits.lock(|cell| {
        let current = cell.get();
        cell.set(current & !bit);
        current & bit != 0
    })
}

fn set_peri_bit(bits: &BlockingMutex<crate::RawMutex, Cell<u32>>, peri_id: usize) {
    let bit = bit_for_peri(peri_id);
    bits.lock(|cell| cell.set(cell.get() | bit));
}

fn required_peripheral_mask() -> u32 {
    if crate::SPLIT_PERIPHERALS_NUM >= u32::BITS as usize {
        u32::MAX
    } else {
        (1u32 << crate::SPLIT_PERIPHERALS_NUM) - 1
    }
}

fn all_peripherals_connected() -> bool {
    CONNECTED_PERIPHERALS.lock(Cell::get) & required_peripheral_mask() == required_peripheral_mask()
}

/// Latch a peripheral's validated link state for the split search window.
fn note_peripheral_connection(id: usize, connected: bool) {
    let bit = bit_for_peri(id);
    CONNECTED_PERIPHERALS.lock(|state| {
        let next = if connected {
            state.get() | bit
        } else {
            state.get() & !bit
        };
        state.set(next);
    });
    set_peripheral_connected(id, connected);
    PERIPHERAL_CONNECTION_CHANGED.signal(());
}

fn publish_split_connection_state(state: SplitConnectionState, generation: u32, terminal: bool) {
    set_current_split_connection_state(state);
    info!(
        "[SPLIT_STATE_L_V15] state={:?} generation={} terminal={}",
        state, generation, terminal
    );
    publish_event(SplitConnectionStateEvent(state));
    if terminal {
        SPLIT_WINDOW_DONE.signal(generation);
    }
}

/// Supervise the complete split-link search window.
///
/// Peripheral sessions keep reconnecting in the background, while this task
/// owns the visible `Searching -> Connected/Idle` state and its timeout.
pub async fn run_split_connection_supervisor() {
    let timeout = Duration::from_secs(u64::from(SPLIT_PAIRING_TIMEOUT_SECONDS));
    let mut generation = SPLIT_WINDOW_GENERATION.lock(Cell::get);
    let mut state = if all_peripherals_connected() {
        SplitConnectionState::Connected
    } else {
        SplitConnectionState::Searching
    };
    let mut deadline = Instant::now() + timeout;
    publish_split_connection_state(state, generation, state == SplitConnectionState::Connected);

    loop {
        match state {
            SplitConnectionState::Searching if SPLIT_PAIRING_TIMEOUT_SECONDS == 0 => {
                match select(PERIPHERAL_CONNECTION_CHANGED.wait(), SPLIT_WINDOW_RESTART.wait()).await {
                    Either::First(()) => {
                        if all_peripherals_connected() {
                            state = SplitConnectionState::Connected;
                            publish_split_connection_state(state, generation, true);
                        }
                    }
                    Either::Second(next_generation) => {
                        generation = next_generation;
                        state = if all_peripherals_connected() {
                            SplitConnectionState::Connected
                        } else {
                            SplitConnectionState::Searching
                        };
                        publish_split_connection_state(state, generation, state == SplitConnectionState::Connected);
                    }
                }
            }
            SplitConnectionState::Searching => {
                match select3(
                    PERIPHERAL_CONNECTION_CHANGED.wait(),
                    SPLIT_WINDOW_RESTART.wait(),
                    Timer::at(deadline),
                )
                .await
                {
                    Either3::First(()) => {
                        if all_peripherals_connected() {
                            state = SplitConnectionState::Connected;
                            publish_split_connection_state(state, generation, true);
                        }
                    }
                    Either3::Second(next_generation) => {
                        generation = next_generation;
                        deadline = Instant::now() + timeout;
                        state = if all_peripherals_connected() {
                            SplitConnectionState::Connected
                        } else {
                            SplitConnectionState::Searching
                        };
                        publish_split_connection_state(state, generation, state == SplitConnectionState::Connected);
                    }
                    Either3::Third(()) => {
                        state = SplitConnectionState::Idle;
                        publish_split_connection_state(state, generation, true);
                    }
                }
            }
            SplitConnectionState::Connected => {
                match select(PERIPHERAL_CONNECTION_CHANGED.wait(), SPLIT_WINDOW_RESTART.wait()).await {
                    Either::First(()) => {
                        if !all_peripherals_connected() {
                            state = SplitConnectionState::Searching;
                            deadline = Instant::now() + timeout;
                            publish_split_connection_state(state, generation, false);
                        }
                    }
                    Either::Second(next_generation) => {
                        generation = next_generation;
                        if all_peripherals_connected() {
                            publish_split_connection_state(state, generation, true);
                        } else {
                            state = SplitConnectionState::Searching;
                            deadline = Instant::now() + timeout;
                            publish_split_connection_state(state, generation, false);
                        }
                    }
                }
            }
            SplitConnectionState::Idle => {
                match select(PERIPHERAL_CONNECTION_CHANGED.wait(), SPLIT_WINDOW_RESTART.wait()).await {
                    Either::First(()) => {
                        if all_peripherals_connected() {
                            state = SplitConnectionState::Connected;
                            publish_split_connection_state(state, generation, true);
                        }
                    }
                    Either::Second(next_generation) => {
                        generation = next_generation;
                        deadline = Instant::now() + timeout;
                        state = if all_peripherals_connected() {
                            SplitConnectionState::Connected
                        } else {
                            SplitConnectionState::Searching
                        };
                        publish_split_connection_state(state, generation, state == SplitConnectionState::Connected);
                    }
                }
            }
        }
    }
}

/// Start a fresh split acquisition phase and wait for either all peripherals
/// or the configured split timeout.
pub(crate) async fn wait_for_split_connection_window() {
    if SPLIT_PAIRING_TIMEOUT_SECONDS == 0 || all_peripherals_connected() {
        return;
    }

    let generation = SPLIT_WINDOW_GENERATION.lock(|state| {
        let next = state.get().wrapping_add(1);
        state.set(next);
        next
    });
    SPLIT_WINDOW_RESTART.signal(generation);
    loop {
        if SPLIT_WINDOW_DONE.wait().await == generation {
            return;
        }
    }
}

/// Scan for peripheral addresses, connect them, and hand each connection to
/// that slot's session; sessions report back on `ended`.
pub(crate) async fn scan_and_connect_peripherals<'a, C: Controller + ControllerCmdSync<LeSetScanParams>>(
    stack: &'a Stack<'_, C, DefaultPacketPool>,
    conns: &[Channel<NoopRawMutex, Connection<'a, DefaultPacketPool>, 1>; crate::SPLIT_PERIPHERALS_NUM],
    ended: &Channel<NoopRawMutex, usize, { crate::SPLIT_PERIPHERALS_NUM }>,
) {
    // Load each peripheral's stored address first.
    let mut peripheral_slots: [SlotState; crate::SPLIT_PERIPHERALS_NUM] = core::array::from_fn(|_| SlotState::NoAddr);
    for (id, slot) in peripheral_slots.iter_mut().enumerate() {
        if let Ok(Some(StorageValue::PeerAddress(peer))) = read(StorageKey::PeerAddress(id as u8)).await
            && peer.is_valid
        {
            *slot = SlotState::Disconnected(peer.address);
        }
    }

    let mut central = stack.central();
    let mut reconnect = SleepReconnect::default();
    wait_for_stack_started().await;
    // The board set these before the stack started, usually before a USB
    // log is attached, so they are reported here.
    if let Some(us) = crate::BLE_CENTRAL_EVENT_LENGTH_US {
        info!("[sdc] central event length {} us", us);
    }
    if let Some(us) = crate::BLE_CENTRAL_ACL_EVENT_SPACING_US {
        info!("[sdc] central ACL event spacing {} us", us);
    }
    loop {
        // Mark ended sessions `Disconnected`, or forget the peer when its
        // session gave up on it.
        while let Ok(id) = ended.try_receive() {
            if let SlotState::Connected(addr) = peripheral_slots[id] {
                peripheral_slots[id] = if take_peri_bit(&FORGOTTEN_PEERS, id) {
                    SlotState::NoAddr
                } else {
                    SlotState::Disconnected(addr)
                };
            }
        }

        // Put every pending address in one accept list: the controller connects
        // to whichever peripheral answers first, so an absent one doesn't block
        // a present one for the whole timeout window.
        let mut pending: heapless::Vec<(usize, [u8; 6]), { crate::SPLIT_PERIPHERALS_NUM }> = heapless::Vec::new();
        for (id, slot) in peripheral_slots.iter().enumerate() {
            if let SlotState::Disconnected(addr) = slot {
                let _ = pending.push((id, *addr));
            }
        }
        if !pending.is_empty() {
            // If there're `Disconnected` peripherals, connect to them first.
            let targets: heapless::Vec<Address, { crate::SPLIT_PERIPHERALS_NUM }> =
                pending.iter().map(|(_, addr)| Address::random(*addr)).collect();
            let config = ConnectConfig {
                connect_params: active_central_conn_param(SplitLinkProfile::Keyboard),
                scan_config: ScanConfig {
                    filter_accept_list: &targets,
                    ..scan_config(SPLIT_CENTRAL_SCAN_WINDOW)
                },
            };
            info!("Start connecting, {} peripheral(s) pending", pending.len());
            let connected = match with_timeout(Duration::from_secs(15), central.connect(&config)).await {
                Ok(Ok(conn)) => {
                    let peer = conn.peer_address();
                    if let Some(&(id, addr)) = pending.iter().find(|(_, addr)| Address::random(*addr) == peer) {
                        info!("Connected to peripheral {}", id);
                        peripheral_slots[id] = SlotState::Connected(addr);
                        conns[id].send(conn).await;
                    } else {
                        warn!("Connected peer {:?} matches no pending slot", peer.addr);
                    }
                    true
                }
                Ok(Err(e)) => {
                    #[cfg(feature = "defmt")]
                    let e = defmt::Debug2Format(&e);
                    error!("Connect error: {:?}", e);
                    Timer::after_millis(500).await;
                    false
                }
                Err(_) => {
                    // None answered.
                    if crate::state::current_sleep_state() {
                        warn!("Connect timeout while asleep, keeping {} address(es)", pending.len());
                    } else {
                        warn!("Connect timeout, clearing {} address(es)", pending.len());
                        for &(id, _) in &pending {
                            peripheral_slots[id] = SlotState::NoAddr;
                        }
                    }
                    false
                }
            };
            reconnect.after_attempt(connected, ended).await;
        } else if peripheral_slots.iter().all(|s| matches!(s, SlotState::Connected(_))) {
            // All peripherals are connected: wait until a session ends.
            ended.ready_to_receive().await;
        } else if crate::state::current_sleep_state() {
            // There's empty peripheral slot, and the central is sleeping,
            // don't scanning, just wait.
            wait_until_wakeup(ended).await;
        } else {
            // Place the `NoAddr` peripherals by scanning for them.
            info!("Start scanning peripherals");
            let session = start_scan(stack, SPLIT_CENTRAL_SCAN_WINDOW, &[]).await;
            let event = with_timeout(
                Duration::from_secs(30),
                select(PERIPHERAL_FOUND.wait(), ended.ready_to_receive()),
            )
            .await;
            // Wait until the controller has confirmed the stop: it refuses an
            // initiator until then.
            session.stop().await;
            info!("Stop scanning");
            if let Ok(Either::First((id, addr))) = event {
                // The id comes off the air — bounds-check it. Keep the first
                // address seen for a slot; an occupied slot is cleared only
                // when connecting to it times out.
                match peripheral_slots.get_mut(id as usize) {
                    Some(slot) if matches!(slot, SlotState::NoAddr) => {
                        let addr = addr.into_inner();
                        info!("Scanned new peripheral {:?}", addr);
                        *slot = SlotState::Disconnected(addr);
                        // Do not persist a scanned address until the GATT product-id
                        // handshake proves that it belongs to this keyboard model.
                        set_peri_bit(&UNCOMMITTED_PEER_CANDIDATES, id as usize);
                    }
                    _ => {}
                }
            }
        }
    }
}

/// The wait before a sleeping central's next attempt at a dropped link, doubled
/// after each failure in a row up to `SLEEP_RECONNECT_MAX`.
const SLEEP_RECONNECT_FIRST: Duration = Duration::from_secs(1);
const SLEEP_RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Paces the connect attempts of a sleeping central. Its peripherals can't wake it
/// while their links are down, so it keeps trying on its own, backing off to keep
/// the radio mostly idle. An awake central retries at once.
#[derive(Default)]
struct SleepReconnect {
    failures: u32,
}

impl SleepReconnect {
    /// Hold off the next attempt after one that failed while the central sleeps.
    /// A wake-up or another ended session cuts the wait short.
    async fn after_attempt(
        &mut self,
        connected: bool,
        ended: &Channel<NoopRawMutex, usize, { crate::SPLIT_PERIPHERALS_NUM }>,
    ) {
        if connected || !crate::state::current_sleep_state() {
            self.failures = 0;
            return;
        }
        let wait = (SLEEP_RECONNECT_FIRST * (1 << self.failures.min(5))).min(SLEEP_RECONNECT_MAX);
        self.failures = self.failures.saturating_add(1);
        let _ = with_timeout(wait, wait_until_wakeup(ended)).await;
    }
}

async fn wait_until_wakeup(ended: &Channel<NoopRawMutex, usize, { crate::SPLIT_PERIPHERALS_NUM }>) {
    while crate::state::current_sleep_state() {
        if with_timeout(Duration::from_secs(1), ended.ready_to_receive())
            .await
            .is_ok()
        {
            return;
        }
    }
}

// When no peripheral address is saved, the central should first scan for peripheral.
// This handler is used to handle the scan result.
pub(crate) struct ScanHandler;

impl EventHandler for ScanHandler {
    fn on_adv_reports(&self, mut it: LeAdvReportsIter<'_>) {
        while let Some(Ok(report)) = it.next() {
            let Some(id) = split_peripheral_id_from_advertisement(report.data) else {
                continue;
            };
            info!("Found split peripheral: id={:?}, addr={:?}", id, report.addr);
            PERIPHERAL_FOUND.signal((id, report.addr));
            break;
        }
    }
}

/// Peripheral id from the advertisement of one of this keyboard's halves, in
/// RMK's format or the previous Ergohaven firmware's: split service UUID plus
/// product-tagged manufacturer data. Another keyboard's half is skipped here:
/// the product check after connecting rejected it, but every scan found it
/// again and kept the central from its own halves.
fn split_peripheral_id_from_advertisement(data: &[u8]) -> Option<u8> {
    match Adv::decode(data) {
        Some(Adv::SplitPeripheral { id, product_id }) => return (product_id == crate::SPLIT_PRODUCT_ID).then_some(id),
        Some(_) => return None,
        None => {}
    }

    let mut has_split_service = false;
    let mut matching_product_peripheral_id = None;
    let mut offset = 0usize;

    while offset < data.len() {
        let len = data[offset] as usize;
        if len == 0 {
            break;
        }
        let end = offset + 1 + len;
        if end > data.len() {
            break;
        }

        let ad_type = data[offset + 1];
        let payload = &data[offset + 2..end];
        match ad_type {
            0x07 if payload == SPLIT_SERVICE_UUID.to_le_bytes() => {
                has_split_service = true;
            }
            0xff if payload.len() >= 5 => {
                let company_id = u16::from_le_bytes([payload[0], payload[1]]);
                let product_id = u16::from_le_bytes([payload[2], payload[3]]);
                if company_id == LEGACY_SPLIT_COMPANY_ID && product_id == crate::SPLIT_PRODUCT_ID {
                    matching_product_peripheral_id = Some(payload[4]);
                }
            }
            _ => {}
        }

        offset = end;
    }

    has_split_service.then_some(matching_product_peripheral_id).flatten()
}

/// Serve one peripheral slot: take each link established by
/// [`scan_and_connect_peripherals`], run the split session over it until it ends, and
/// report back so the radio reconnects or rediscovers the peripheral.
pub(crate) async fn run_peripheral_session<
    'a,
    #[cfg(not(feature = "subrating"))] C: Controller
        + ControllerCmdSync<LeSetScanParams>
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>,
    #[cfg(feature = "subrating")] C: Controller
        + ControllerCmdSync<LeSetScanParams>
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>
        + ControllerCmdAsync<LeSubrateRequest>,
>(
    id: usize,
    conns: &Channel<NoopRawMutex, Connection<'a, DefaultPacketPool>, 1>,
    ended: &Channel<NoopRawMutex, usize, { crate::SPLIT_PERIPHERALS_NUM }>,
    stack: &'a Stack<'_, C, DefaultPacketPool>,
    matrix_config: PeripheralMatrixConfig,
) {
    trace!("SPLIT_MESSAGE_MAX_SIZE: {}", SPLIT_MESSAGE_MAX_SIZE);
    let mut peer_retry_state = PeerRetryState::default();
    loop {
        let conn = conns.receive().await;
        let peer_validated = Cell::new(false);
        if let Err(e) = run_central_manager_task(id, stack, &conn, matrix_config, &peer_validated).await {
            #[cfg(feature = "defmt")]
            let e = defmt::Debug2Format(&e);
            error!("BLE central error: {:?}", e);
        }
        note_peripheral_connection(id, false);
        if peer_validated.get() {
            peer_retry_state.on_validated();
        } else {
            warn!("Split peripheral {} disconnected before validation", id);
            let uncommitted = take_peri_bit(&UNCOMMITTED_PEER_CANDIDATES, id);
            match peer_retry_state.on_failed_attempt(uncommitted) {
                FailedPeerAction::Retain { consecutive_failures } => warn!(
                    "Retaining validated split peer {} after transient connection failure {}/{}",
                    id, consecutive_failures, VALIDATED_PEER_FAILURE_LIMIT
                ),
                // An address learned by the current scan is not trusted until product
                // validation succeeds, so forget it immediately. A previously validated
                // peer gets a bounded retry window before it is cleared, allowing both
                // transient recovery and eventual replacement/re-pairing.
                FailedPeerAction::Forget => {
                    store_unchecked(StorageItem::PeerAddress(PeerAddress::new(id as u8, false, [0; 6]))).await;
                    set_peri_bit(&FORGOTTEN_PEERS, id);
                }
            }
        }
        // Dropping the last handle files a disconnect request that the stack
        // runner serves on its own; the pause lets that finish and lets the
        // peripheral advertise again, so the reconnect finds a clean state.
        drop(conn);
        Timer::after_millis(500).await;
        ended.send(id).await;
    }
}

fn active_central_conn_param(profile: SplitLinkProfile) -> RequestedConnParams {
    let interval = match profile {
        SplitLinkProfile::Keyboard => Duration::from_millis(15),
        SplitLinkProfile::Pointing => Duration::from_micros(7_500),
    };
    RequestedConnParams {
        min_connection_interval: interval,
        max_connection_interval: interval,
        // Active split links must attend every event. In particular, a
        // pointing link cannot sustain 125 Hz with peripheral latency.
        max_latency: 0,
        supervision_timeout: Duration::from_secs(5),
        ..Default::default()
    }
}

#[cfg(not(feature = "subrating"))]
fn sleeping_central_conn_param(profile: SplitLinkProfile) -> RequestedConnParams {
    let usb_powered = matches!(
        crate::state::active_transport(),
        Some(crate::types::connection::ConnectionType::Usb)
    );
    sleeping_central_conn_param_for(profile, usb_powered)
}

#[cfg(not(feature = "subrating"))]
fn sleeping_central_conn_param_for(profile: SplitLinkProfile, usb_powered: bool) -> RequestedConnParams {
    // Slave latency keeps the peripheral's effective idle cadence at about
    // 210 ms either way, so its sleeping cost does not depend on the base
    // interval. A short base interval lets a peripheral with queued input
    // attend the next connection event promptly, and it also shortens the
    // wake-up: the update back to the active cadence lands on an instant the
    // controller schedules a fixed number of connection events ahead, and
    // motion reports arrive once per event until then. Only a USB-powered
    // central can afford to listen every 15 ms while the halves sleep; a
    // battery central keeps 30 ms unless the host-first wake profile asks for
    // the short anchor on pointing links.
    #[cfg(feature = "host_first_split_wake")]
    let battery = match profile {
        SplitLinkProfile::Keyboard => (30, 6),
        SplitLinkProfile::Pointing => (15, 13),
    };
    #[cfg(not(feature = "host_first_split_wake"))]
    let battery = {
        let _ = profile;
        (30, 6)
    };
    let (interval, max_latency) = if usb_powered { (15, 13) } else { battery };
    RequestedConnParams {
        min_connection_interval: Duration::from_millis(interval),
        max_connection_interval: Duration::from_millis(interval),
        max_latency,
        supervision_timeout: Duration::from_secs(11),
        ..Default::default()
    }
}

#[cfg(all(not(feature = "subrating"), feature = "host_first_split_wake"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SplitWakeOrderAction {
    Proceed,
    WaitForHost,
}

#[cfg(all(not(feature = "subrating"), feature = "host_first_split_wake"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SplitLinkUpdatePlan {
    None,
    Sleeping(SplitLinkProfile),
    Active(SplitLinkProfile),
}

#[cfg(all(not(feature = "subrating"), feature = "host_first_split_wake"))]
const fn split_wake_order_action(
    profile: SplitLinkProfile,
    link_was_sleeping: bool,
    host_gate: HostWakeOrderGate,
) -> SplitWakeOrderAction {
    if link_was_sleeping
        && matches!(profile, SplitLinkProfile::Pointing)
        && matches!(host_gate, HostWakeOrderGate::Pending)
    {
        SplitWakeOrderAction::WaitForHost
    } else {
        SplitWakeOrderAction::Proceed
    }
}

#[cfg(all(not(feature = "subrating"), feature = "host_first_split_wake"))]
fn split_link_update_plan(
    applied_profile: SplitLinkProfile,
    applied_sleeping: bool,
    current_profile: SplitLinkProfile,
    current_sleeping: bool,
) -> SplitLinkUpdatePlan {
    if current_sleeping == applied_sleeping && current_profile == applied_profile {
        SplitLinkUpdatePlan::None
    } else if current_sleeping {
        SplitLinkUpdatePlan::Sleeping(current_profile)
    } else {
        SplitLinkUpdatePlan::Active(current_profile)
    }
}

async fn validate_split_product<T: SplitReader + SplitWriter>(driver: &mut T) -> bool {
    if let Err(e) = driver.write(&SplitMessage::ProductId(crate::SPLIT_PRODUCT_ID)).await {
        warn!("Split product check write failed: {:?}", e);
        return false;
    }

    match with_timeout(Duration::from_millis(1500), async {
        loop {
            match driver.read().await {
                Ok(SplitMessage::ProductId(product_id)) if product_id == crate::SPLIT_PRODUCT_ID => return true,
                Ok(SplitMessage::ProductId(product_id)) => {
                    warn!(
                        "Split product id mismatch: got {}, expected {}",
                        product_id,
                        crate::SPLIT_PRODUCT_ID
                    );
                    return false;
                }
                Ok(message) => debug!("Ignoring pre-handshake split message: {:?}", message),
                Err(e) => {
                    warn!("Split product check read failed: {:?}", e);
                    return false;
                }
            }
        }
    })
    .await
    {
        Ok(valid) => valid,
        Err(_) => {
            warn!("Split product check timeout");
            false
        }
    }
}

#[cfg(feature = "subrating")]
pub(crate) mod subrating {
    // Measurements on nrf52840 for subrate request parameters:
    //    |-------+------+-----+------+---------+------------|---------|
    //    |   HCL | [ms] |  SF | [ms] | IC [µA] |  KPL [ms]  | IP [µA] |
    //    |-------+------+-----+------+---------+------------|---------|
    //    |    60 |  450 |  10 |   75 |      80 |  41 /   82 |      21 |
    //    |    30 |  225 |  30 |  225 |      75 | 116 /  232 |      21 |
    // ==>|    60 |  450 |  30 |  225 |      59 | 116 /  232 |      21 |<== Connected Sleep
    //    |   180 | 1350 |  30 |  225 |      48 | 116 /  232 |      21 |
    //    |   300 | 2250 |  30 |  225 |      48 | 116 /  232 |      21 |
    //    |    30 |  225 |  60 |  450 |      63 | 228 /  457 |      21 |
    //    |    60 |  450 |  60 |  450 |      48 | 228 /  457 |      21 |
    //    |   180 | 1350 |  60 |  450 |      39 | 228 /  457 |      21 |
    //    |   300 | 2250 |  60 |  450 |      34 | 228 /  457 |      21 |
    //    |   300 | 2250 | 120 |  900 |      32 | 453 /  907 |      21 |
    // ==>| no HC |      | 100 |  750 |      24 | 378 /  757 |      21 |<== Disconnected Sleep
    //    | no HC |      | 125 |  937 |      22 | 472 /  945 |      21 |
    //    | no HC |      | 250 | 1875 |      21 | 941 / 1882 |      24 |
    //    |-------+------+-----+------+---------+------------|---------|
    //    HCL .. Host Connection max latency (host <-> central, assumes 7.5ms interval)
    //    SF ... Subrate Factor split connection
    //    IC ... Central average current
    //    KPL .. Key Press latency (mean/worst)
    //    IP ... Peripheral average current
    //
    //
    // In active mode without pressing any key, the peripheral current depends on the max
    // latency of the split connection:
    //    | max_latency |  [ms] | min_timeout [ms] | IP [µA] |
    //    |-------------+-------+------------------+---------|
    //    |          10 |    75 |              165 |      72 |
    // ==>|          30 |   225 |              465 |      38 |<== Default Params
    //    |          60 |   450 |              915 |      30 |
    //    |         300 |  2250 |             4515 |      21 |
    //    |-------------+-------+------------------+---------|

    use bt_hci::cmd::le::{LeSubrateRequest, LeSubrateRequestParams};
    use bt_hci::controller::ControllerCmdAsync;
    use bt_hci::param::{ConnHandle, Duration, Error as HciError};
    use trouble_host::prelude::*;

    const SLEEP_HOST_CONN_SUBRATE: u16 = 30;
    const SLEEP_NO_HOST_SUBRATE: u16 = 100;

    // In some cases, the subrate request procedure does not complete with only one continuation.
    const SLEEP_CONTINUATION_NUMBER: u16 = 2;

    const fn calc_max_latency(subrate_max: u16) -> u16 {
        // BLE spec requires: Subrate_Max * (Max_Latency + 1) <= 500.
        // We use 250 here to tolerant clock drift(max 500 ppm).
        (250 / subrate_max) - 1
    }

    /// Default subrating params when the central is awake.
    pub(super) fn default_split_subrating_params(handle: ConnHandle) -> LeSubrateRequestParams {
        LeSubrateRequestParams {
            handle,
            subrate_min: 1,
            subrate_max: 1,
            max_latency: 30,
            continuation_number: 0,
            supervision_timeout: Duration::from_secs(6),
        }
    }

    pub(super) fn sleep_split_subrating_params(handle: ConnHandle) -> LeSubrateRequestParams {
        let subrate = if crate::state::active_transport().is_some() {
            SLEEP_HOST_CONN_SUBRATE
        } else {
            SLEEP_NO_HOST_SUBRATE
        };

        LeSubrateRequestParams {
            handle,
            subrate_min: subrate,
            subrate_max: subrate,
            max_latency: calc_max_latency(subrate), // answers every 3600ms with a host, 3750ms without
            continuation_number: SLEEP_CONTINUATION_NUMBER,
            supervision_timeout: Duration::from_millis(15_000),
        }
    }

    pub(crate) async fn update_subrate_factor<C: Controller + ControllerCmdAsync<LeSubrateRequest>, P: PacketPool>(
        stack: &Stack<'_, C, P>,
        params: LeSubrateRequestParams,
    ) -> bool {
        for _ in 0..10 {
            let subrate_request = LeSubrateRequest::from(params);
            match stack.async_command(subrate_request).await {
                Ok(_) => {
                    debug!("[update_subrate_factor] requested {:?}", params);
                    return true;
                }
                Err(BleHostError::BleHost(Error::Hci(error))) => {
                    // A connection runs one link-layer control procedure at a time, and
                    // a fresh one is still running its own.
                    if error == HciError::CONTROLLER_BUSY || error == HciError::DIFFERENT_TRANSACTION_COLLISION {
                        info!("[update_subrate_factor] controller busy, retrying: {:?}", error);
                        embassy_time::Timer::after_millis(100).await;
                        continue;
                    }
                    if error == HciError::UNKNOWN_CONN_IDENTIFIER {
                        error!("[update_subrate_factor] stale split link, rebooting");
                        embassy_time::Timer::after_millis(100).await;
                        crate::boot::reboot_keyboard();
                    }
                    error!("[update_subrate_factor] HCI error: {:?}", error);
                    return false;
                }
                Err(e) => {
                    #[cfg(feature = "defmt")]
                    let e = defmt::Debug2Format(&e);
                    error!("[update_subrate_factor] BLE host error: {:?}", e);
                    return false;
                }
            }
        }
        warn!("[update_subrate_factor] controller stayed busy, giving up");
        false
    }
}

async fn run_central_manager_task<
    'b,
    's: 'b,
    #[cfg(not(feature = "subrating"))] C: Controller + ControllerCmdAsync<LeSetPhy> + ControllerCmdSync<LeReadLocalSupportedFeatures>,
    #[cfg(feature = "subrating")] C: Controller
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>
        + ControllerCmdAsync<LeSubrateRequest>,
    P: PacketPool,
>(
    id: usize,
    stack: &'b Stack<'s, C, P>,
    conn: &Connection<'b, P>,
    matrix_config: PeripheralMatrixConfig,
    peer_validated: &Cell<bool>,
) -> Result<(), BleHostError<C::Error>> {
    let client = GattClient::<C, P, 10>::new(stack, conn).await?;

    if crate::BLE_USE_2M_PHY {
        update_ble_phy(stack, conn, PhyKind::Le2M).await;
    } else {
        info!("Split link preserving configured LE 1M PHY");
    }

    // Without either of these features the follower sets the first parameters
    // itself, from the latched sleep state: an active request here would race
    // the sleep request of a link that reconnects while the keyboard sleeps.
    #[cfg(any(feature = "subrating", feature = "host_first_split_wake"))]
    {
        info!("Updating connection parameters for peripheral");
        update_conn_params(stack, conn, &active_central_conn_param(split_link_profile(id))).await;
    }

    let peer = conn.peer_address().addr.into_inner();
    let (Either3::First(e) | Either3::Second(e) | Either3::Third(e)) = select3(
        ble_central_task(&client, conn),
        discover_and_run_manager(id, peer, &client, matrix_config, peer_validated),
        update_conn_params_on_sleep_change(stack, conn, id),
    )
    .await;
    e
}

async fn ble_central_task<'a, C: Controller + ControllerCmdAsync<LeSetPhy>, P: PacketPool>(
    client: &GattClient<'a, C, P, 10>,
    conn: &Connection<'a, P>,
) -> Result<(), BleHostError<C::Error>> {
    // Watch for the disconnect; draining the other events keeps the small
    // per-connection queue from sitting full and dropping it.
    let conn_events = async {
        loop {
            if let ConnectionEvent::Disconnected { reason } = conn.next().await {
                info!("Connection lost: {:?}", reason);
                break;
            }
        }
    };

    match select(client.task(), conn_events).await {
        Either::First(e) => e,
        Either::Second(()) => Ok(()),
    }
}

/// Discover the split service on the connected peripheral, check that it
/// belongs to this keyboard model, then run its [`PeripheralManager`] over the
/// GATT link.
async fn discover_and_run_manager<C: Controller + ControllerCmdAsync<LeSetPhy>, P: PacketPool>(
    id: usize,
    peer: [u8; 6],
    client: &GattClient<'_, C, P, 10>,
    matrix_config: PeripheralMatrixConfig,
    peer_validated: &Cell<bool>,
) -> Result<(), BleHostError<C::Error>> {
    let services = client
        .services_by_uuid(&Uuid::new_long(SPLIT_SERVICE_UUID.to_le_bytes()))
        .await?;
    info!("Services found");
    let Some(service) = services.first() else {
        return Ok(());
    };
    let message_to_central = client
        .characteristic_by_uuid::<GattSplitMessage>(service, &Uuid::new_long(MESSAGE_TO_CENTRAL_UUID.to_le_bytes()))
        .await?;
    info!("Message to central found");
    let message_to_peripheral = client
        .characteristic_by_uuid::<GattSplitMessage>(service, &Uuid::new_long(MESSAGE_TO_PERIPHERAL_UUID.to_le_bytes()))
        .await?;
    info!("Subscribing notifications");
    let listener = client.subscribe(&message_to_central, false).await?;
    let mut split_ble_driver = BleSplitCentralDriver {
        listener,
        message_to_peripheral,
        client,
        id,
    };
    if !validate_split_product(&mut split_ble_driver).await {
        warn!("Rejecting split peripheral {} after product validation", id);
        return Ok(());
    }
    peer_validated.set(true);
    if take_peri_bit(&UNCOMMITTED_PEER_CANDIDATES, id) {
        store_unchecked(StorageItem::PeerAddress(PeerAddress::new(id as u8, true, peer))).await;
    }
    note_peripheral_connection(id, true);

    #[cfg(not(feature = "custom_message"))]
    PeripheralManager::new(split_ble_driver, id, matrix_config).run().await;
    // A peripheral built without `custom_message` has no such characteristics;
    #[cfg(feature = "custom_message")]
    {
        use postcard::experimental::max_size::MaxSize;

        use crate::custom_message::{CustomMessage, CustomMessageTarget, forward, send};

        let to_central = client
            .characteristic_by_uuid::<heapless::Vec<u8, { CustomMessage::POSTCARD_MAX_SIZE }>>(
                service,
                &Uuid::new_long(CUSTOM_TO_CENTRAL_UUID.to_le_bytes()),
            )
            .await
            .ok();
        let to_peripheral = client
            .characteristic_by_uuid::<heapless::Vec<u8, { CustomMessage::POSTCARD_MAX_SIZE }>>(
                service,
                &Uuid::new_long(CUSTOM_TO_PERIPHERAL_UUID.to_le_bytes()),
            )
            .await
            .ok();
        let mut incoming = match &to_central {
            Some(characteristic) => Some(client.subscribe(characteristic, false).await?),
            None => None,
        };

        let from_peripheral = async {
            match incoming.as_mut() {
                None => core::future::pending().await,
                Some(listener) => loop {
                    let notification = listener.next().await;
                    match postcard::from_bytes::<CustomMessage>(notification.as_ref()) {
                        // A central is the one board with links on both sides: it relays
                        // what is headed past it and delivers what names it.
                        Ok(message) => match message.target {
                            CustomMessageTarget::Dongle => send(message),
                            CustomMessageTarget::Central => publish_event(message),
                            CustomMessageTarget::Peripherals => (),
                        },
                        Err(_) => warn!("[split] undecodable custom message dropped"),
                    }
                },
            }
        };

        let to_this_peripheral = async {
            let Some(characteristic) = to_peripheral.as_ref() else {
                return core::future::pending().await;
            };
            forward(Some(CustomMessageTarget::Peripherals), async |encoded| {
                client
                    .write_characteristic_without_response(characteristic, encoded)
                    .await
            })
            .await
        };

        select3(
            PeripheralManager::new(split_ble_driver, id, matrix_config).run(),
            from_peripheral,
            to_this_peripheral,
        )
        .await;
    }
    info!("Peripheral manager stopped");
    Ok(())
}

/// [`SplitReader`]/[`SplitWriter`] over the peripheral's GATT link: reads are
/// notifications on `message_to_central`, writes go to `message_to_peripheral`.
struct BleSplitCentralDriver<'a, 'b, 'c, C: Controller + ControllerCmdAsync<LeSetPhy>, P: PacketPool> {
    listener: NotificationListener<'b, { trouble_host::config::GATT_CLIENT_NOTIFICATION_MTU }>,
    message_to_peripheral: Characteristic<GattSplitMessage>,
    client: &'c GattClient<'a, C, P, 10>,
    /// Split peripheral id, for the per-link pointing hold
    id: usize,
}

impl<'a, 'b, 'c, C: Controller + ControllerCmdAsync<LeSetPhy>, P: PacketPool> SplitReader
    for BleSplitCentralDriver<'a, 'b, 'c, C, P>
{
    async fn read(&mut self) -> Result<SplitMessage, SplitDriverError> {
        let data = self.listener.next().await;
        let message = postcard::from_bytes(data.as_ref()).map_err(|_| SplitDriverError::DeserializeError)?;
        trace!("Received split message: {:?}", message);

        // Input from the peripheral counts as activity for sleep management.
        // Pointing reports count only when they carry user motion, so sensor
        // noise cannot keep the keyboard awake.
        match &message {
            SplitMessage::Pointing(event) => {
                #[cfg(feature = "rtt_diag")]
                crate::rtt_diag::record_split_rx(event);
                let now_ms = Instant::now().as_millis() as u32;
                if event.has_relative_xy_motion(POINTING_ACTIVITY_THRESHOLD) {
                    LAST_POINTING_ACTIVITY_MS.store(now_ms, Ordering::Release);
                    note_link_pointing_activity(self.id, now_ms);
                }
                if event.is_user_activity() {
                    report_activity();
                }
            }
            SplitMessage::Key(_) => report_activity(),
            _ => {}
        }

        Ok(message)
    }
}

impl<'a, 'b, 'c, C: Controller + ControllerCmdAsync<LeSetPhy>, P: PacketPool> SplitWriter
    for BleSplitCentralDriver<'a, 'b, 'c, C, P>
{
    async fn write(&mut self, message: &SplitMessage) -> Result<usize, SplitDriverError> {
        let gatt_msg = GattSplitMessage::try_from(message)?;
        if let Err(e) = self
            .client
            .write_characteristic_without_response(&self.message_to_peripheral, gatt_msg.as_gatt())
            .await
        {
            if let BleHostError::BleHost(Error::NotFound) = e {
                error!("Peripheral disconnected");
                return Err(SplitDriverError::Disconnected);
            }
            #[cfg(feature = "defmt")]
            let e = defmt::Debug2Format(&e);
            error!("BLE message_to_peripheral_write error: {:?}", e);
        }

        Ok(gatt_msg.len)
    }
}

/// Keep one peripheral link's subrating in sync with the keyboard's sleep
/// state. Runs for as long as the link is up.
#[cfg(feature = "subrating")]
async fn update_conn_params_on_sleep_change<
    'b,
    's: 'b,
    C: Controller + ControllerCmdAsync<LeSetPhy> + ControllerCmdAsync<LeSubrateRequest>,
    P: PacketPool,
>(
    stack: &'b Stack<'s, C, P>,
    conn: &Connection<'b, P>,
    _peripheral_id: usize,
) -> Result<(), BleHostError<C::Error>> {
    let mut sleep_events = SleepStateEvent::subscriber();

    let mut sleeping = crate::state::current_sleep_state();
    if !sleeping {
        // Restart the idle timeout so service discovery isn't cut short. Asleep
        // this isn't user activity, so discovery runs on the sleep parameters.
        report_activity();
    }

    let mut sleeping_conn_param_applied = false;

    loop {
        if sleeping != sleeping_conn_param_applied {
            let params = if sleeping {
                subrating::sleep_split_subrating_params(conn.handle())
            } else {
                subrating::default_split_subrating_params(conn.handle())
            };
            if subrating::update_subrate_factor(stack, params).await {
                sleeping_conn_param_applied = sleeping;
            }
        }

        sleeping = sleep_events.next_event().await.0;
    }
}

/// Keep one split link synchronized with the keyboard-wide sleep manager.
/// The manager outlives every connection; this follower is recreated with the
/// link and owns only that link's connection parameters.
#[cfg(all(not(feature = "subrating"), feature = "host_first_split_wake"))]
async fn update_conn_params_on_sleep_change<
    'b,
    's: 'b,
    C: Controller + ControllerCmdAsync<LeSetPhy> + ControllerCmdSync<LeReadLocalSupportedFeatures>,
    P: PacketPool,
>(
    stack: &'b Stack<'s, C, P>,
    conn: &Connection<'b, P>,
    peripheral_id: usize,
) -> Result<(), BleHostError<C::Error>> {
    let mut sleep_events = SleepStateEvent::subscriber();
    let profile_changed = LINK_PROFILE_CHANGED.get(peripheral_id);

    // A new link must follow the already-latched keyboard state. Treating link
    // creation as user input while asleep can wake the host and every other
    // split link.
    let mut applied_profile = split_link_profile(peripheral_id);
    let mut applied_sleeping = if crate::state::current_sleep_state() {
        info!("New split link inherits sleep mode");
        update_conn_params(stack, conn, &sleeping_central_conn_param(applied_profile)).await
    } else {
        // Restart the idle timeout so service discovery isn't cut short.
        report_activity();
        false
    };
    loop {
        let update = if let Some(profile_changed) = profile_changed {
            select(sleep_events.next_event(), profile_changed.wait()).await
        } else {
            Either::First(sleep_events.next_event().await)
        };
        match update {
            Either::First(event) => {
                let mut sleeping = event.0;
                if sleeping == applied_sleeping {
                    continue;
                }

                let mut profile = split_link_profile(peripheral_id);
                if !sleeping
                    && split_wake_order_action(profile, applied_sleeping, host_wake_order_gate())
                        == SplitWakeOrderAction::WaitForHost
                {
                    info!(
                        "[WAKE_ORDER_V30G] split_id={} event=wake state=waiting_for_host_active",
                        peripheral_id
                    );
                    wait_for_host_wake_order_gate().await;

                    // Sleep state and runtime profile can both change while
                    // the host gate is pending. Re-read the authoritative
                    // values before choosing params; never apply the stale
                    // active target captured by the original wake event.
                    sleeping = crate::state::current_sleep_state();
                    profile = split_link_profile(peripheral_id);
                    info!(
                        "[WAKE_ORDER_V30G] split_id={} event=wake state=gate_open_revalidated",
                        peripheral_id
                    );
                }
                let conn_params = match split_link_update_plan(applied_profile, applied_sleeping, profile, sleeping) {
                    SplitLinkUpdatePlan::None => continue,
                    SplitLinkUpdatePlan::Sleeping(profile) => {
                        info!("Split link entering sleep mode");
                        sleeping_central_conn_param(profile)
                    }
                    SplitLinkUpdatePlan::Active(profile) => {
                        info!("Split link restoring active mode");
                        active_central_conn_param(profile)
                    }
                };
                if update_conn_params(stack, conn, &conn_params).await {
                    applied_sleeping = sleeping;
                    applied_profile = profile;
                }
            }
            Either::Second(_) => {
                let profile = split_link_profile(peripheral_id);
                if profile == applied_profile {
                    continue;
                }
                let conn_params = if applied_sleeping {
                    info!("Split link applying runtime sleep profile");
                    sleeping_central_conn_param(profile)
                } else {
                    info!("Split link applying runtime active profile");
                    active_central_conn_param(profile)
                };
                if update_conn_params(stack, conn, &conn_params).await {
                    applied_profile = profile;
                }
            }
        }
    }
}

/// Whether the live parameters of a link are exactly the requested tuple.
#[cfg(all(not(feature = "subrating"), not(feature = "host_first_split_wake")))]
fn split_link_params_live(live: &ConnParams, params: &RequestedConnParams) -> bool {
    live.conn_interval == params.max_connection_interval
        && live.peripheral_latency == params.max_latency
        && live.supervision_timeout == params.supervision_timeout
}

/// Request new parameters for one split link and wait until the controller
/// reports them live. The update lands on an instant a few connection events
/// ahead; treating the request as applied before that has left links on the
/// wrong cadence with nothing to correct them.
///
/// A link already running the requested tuple gets no request at all: the
/// confirmation below would otherwise be satisfied by the pre-request value
/// while the redundant procedure is still in flight, and a following update
/// could overlap it. Any other match can only come from a completed procedure.
#[cfg(all(not(feature = "subrating"), not(feature = "host_first_split_wake")))]
async fn apply_split_link_params<
    'b,
    's: 'b,
    C: Controller + ControllerCmdSync<LeReadLocalSupportedFeatures>,
    P: PacketPool,
>(
    stack: &'b Stack<'s, C, P>,
    conn: &Connection<'b, P>,
    params: &RequestedConnParams,
) -> bool {
    if split_link_params_live(&conn.params(), params) {
        info!(
            "Split link params already live: {} us latency {}",
            params.max_connection_interval.as_micros(),
            params.max_latency
        );
        return true;
    }
    let started = Instant::now();
    if !update_conn_params(stack, conn, params).await {
        return false;
    }
    let deadline = started + Duration::from_secs(2);
    loop {
        let live = conn.params();
        if split_link_params_live(&live, params) {
            info!(
                "Split link params live: {} us latency {} after {} ms",
                live.conn_interval.as_micros(),
                live.peripheral_latency,
                started.elapsed().as_millis()
            );
            return true;
        }
        if Instant::now() >= deadline {
            warn!(
                "Split link params not confirmed: requested {} us latency {}, live {} us latency {}",
                params.max_connection_interval.as_micros(),
                params.max_latency,
                live.conn_interval.as_micros(),
                live.peripheral_latency
            );
            return false;
        }
        Timer::after_millis(20).await;
    }
}

/// Where one split link should be: asleep or awake, and on which cadence.
#[cfg(all(not(feature = "subrating"), not(feature = "host_first_split_wake")))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SplitLinkTarget {
    sleeping: bool,
    profile: SplitLinkProfile,
}

#[cfg(all(not(feature = "subrating"), not(feature = "host_first_split_wake")))]
impl SplitLinkTarget {
    fn conn_params(self) -> RequestedConnParams {
        if self.sleeping {
            sleeping_central_conn_param(self.profile)
        } else {
            active_central_conn_param(self.profile)
        }
    }
}

/// The target of one link right now, from the latched sleep state and the
/// link's configured profile and pointing hold.
#[cfg(all(not(feature = "subrating"), not(feature = "host_first_split_wake")))]
fn split_link_target(peripheral_id: usize, sleeping: bool, now_ms: u32) -> SplitLinkTarget {
    let profile = if sleeping {
        split_link_profile(peripheral_id)
    } else {
        target_split_link_profile(peripheral_id, now_ms)
    };
    SplitLinkTarget { sleeping, profile }
}

/// Record that `target` is what a link now runs. A relaxed pointing link is
/// restored by the next motion report, so this must run even when reaching the
/// target needed no update, e.g. a runtime Keyboard to Pointing switch whose
/// hold has already expired.
#[cfg(all(not(feature = "subrating"), not(feature = "host_first_split_wake")))]
fn note_split_link_target(peripheral_id: usize, target: SplitLinkTarget) {
    if !target.sleeping {
        set_applied_split_link_profile(peripheral_id, target.profile);
    }
}

#[cfg(all(not(feature = "subrating"), not(feature = "host_first_split_wake")))]
const SPLIT_LINK_RETRY_FIRST: Duration = Duration::from_secs(1);
#[cfg(all(not(feature = "subrating"), not(feature = "host_first_split_wake")))]
const SPLIT_LINK_RETRY_MAX: Duration = Duration::from_secs(8);

#[cfg(all(not(feature = "subrating"), not(feature = "host_first_split_wake")))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SplitLinkStep {
    /// The live parameters already serve the target.
    Settled,
    /// Request the target's parameters.
    Apply,
    /// A request for this very target failed; retry once this has elapsed.
    RetryIn(Duration),
}

/// Retry ownership for one split link, kept apart from the radio so every
/// transition is testable. The target is recomputed from authoritative state
/// on every step, never carried over from the event that woke the follower,
/// and `applied` holds only parameters confirmed live. A failed request keeps
/// the previous `applied` and arms a backoff for that target; a different
/// target is requested at once, so a transition cancelled by the next one
/// never delays it.
#[cfg(all(not(feature = "subrating"), not(feature = "host_first_split_wake")))]
#[derive(Default)]
struct SplitLinkFollower {
    applied: Option<(SplitLinkTarget, RequestedConnParams)>,
    failed: Option<(SplitLinkTarget, Instant, u32)>,
}

#[cfg(all(not(feature = "subrating"), not(feature = "host_first_split_wake")))]
impl SplitLinkFollower {
    fn step(&mut self, target: SplitLinkTarget, now: Instant) -> SplitLinkStep {
        let params = target.conn_params();
        if let Some((applied, live)) = &mut self.applied
            && *live == params
        {
            // Profiles that share parameters (both sleeping profiles without
            // host-first wake) change the target without an update.
            *applied = target;
            self.failed = None;
            return SplitLinkStep::Settled;
        }
        match self.failed {
            Some((failed, retry_at, _)) if failed == target && now < retry_at => SplitLinkStep::RetryIn(retry_at - now),
            _ => SplitLinkStep::Apply,
        }
    }

    fn on_result(&mut self, target: SplitLinkTarget, confirmed: bool, now: Instant) {
        if confirmed {
            self.applied = Some((target, target.conn_params()));
            self.failed = None;
            return;
        }
        let failures = match self.failed {
            Some((failed, _, failures)) if failed == target => failures.saturating_add(1),
            _ => 1,
        };
        let backoff = SPLIT_LINK_RETRY_FIRST
            .checked_mul(1 << (failures - 1).min(3))
            .unwrap_or(SPLIT_LINK_RETRY_MAX)
            .min(SPLIT_LINK_RETRY_MAX);
        self.failed = Some((target, now + backoff, failures));
    }

    fn applied(&self) -> Option<SplitLinkTarget> {
        self.applied.as_ref().map(|(target, _)| *target)
    }
}

/// Keep one split link synchronized with the keyboard-wide sleep manager and
/// with its pointing hold. The manager outlives every connection; this
/// follower is recreated with the link, sets its first parameters from the
/// latched sleep state and owns only that link's connection parameters.
#[cfg(all(not(feature = "subrating"), not(feature = "host_first_split_wake")))]
async fn update_conn_params_on_sleep_change<
    'b,
    's: 'b,
    C: Controller + ControllerCmdAsync<LeSetPhy> + ControllerCmdSync<LeReadLocalSupportedFeatures>,
    P: PacketPool,
>(
    stack: &'b Stack<'s, C, P>,
    conn: &Connection<'b, P>,
    peripheral_id: usize,
) -> Result<(), BleHostError<C::Error>> {
    let mut sleep_events = SleepStateEvent::subscriber();
    let profile_changed = LINK_PROFILE_CHANGED.get(peripheral_id);
    let pointing_wake = &LINK_POINTING_WAKE[peripheral_id.min(31)];
    // Motion latched during the previous connection is not this link's.
    pointing_wake.reset();

    // A new link must follow the already-latched keyboard state. Treating link
    // creation as user input while asleep can wake the host and every other
    // split link.
    if !crate::state::current_sleep_state() {
        // Restart the idle timeout so service discovery isn't cut short.
        report_activity();
    }
    let mut follower = SplitLinkFollower::default();
    loop {
        let now = Instant::now();
        let target = split_link_target(
            peripheral_id,
            crate::state::current_sleep_state(),
            now.as_millis() as u32,
        );
        let wait = match follower.step(target, now) {
            SplitLinkStep::Apply => {
                match (follower.applied().map(|applied| applied.sleeping), target.sleeping) {
                    (None, true) => info!("New split link inherits sleep mode"),
                    (None, false) => info!("Updating connection parameters for peripheral"),
                    (Some(false), true) => info!("Split link entering sleep mode"),
                    (Some(true), false) => info!("Split link restoring active mode"),
                    (Some(true), true) => info!("Split link applying runtime sleep profile"),
                    (Some(false), false) if target.profile == SplitLinkProfile::Pointing => {
                        info!("Split link restoring pointing cadence")
                    }
                    (Some(false), false) => info!("Split link relaxing to keyboard cadence"),
                }
                let confirmed = apply_split_link_params(stack, conn, &target.conn_params()).await;
                follower.on_result(target, confirmed, Instant::now());
                if confirmed {
                    note_split_link_target(peripheral_id, target);
                }
                // Re-read the state: it may have moved while the update ran.
                continue;
            }
            SplitLinkStep::Settled => {
                note_split_link_target(peripheral_id, target);
                (!target.sleeping && target.profile == SplitLinkProfile::Pointing)
                    .then(|| pointing_link_hold_remaining(peripheral_id, Instant::now().as_millis() as u32))
            }
            SplitLinkStep::RetryIn(remaining) => {
                warn!("Split link params pending, retrying in {} ms", remaining.as_millis());
                Some(remaining)
            }
        };
        select4(
            sleep_events.next_event(),
            async {
                match profile_changed {
                    Some(changed) => changed.wait().await,
                    None => core::future::pending::<()>().await,
                }
            },
            pointing_wake.wait(),
            async {
                match wait {
                    Some(remaining) => Timer::after(remaining).await,
                    None => core::future::pending::<()>().await,
                }
            },
        )
        .await;
    }
}

fn quiet_period_remaining(now_ms: u32, last_activity_ms: u32, quiet_period: Duration) -> Duration {
    if last_activity_ms == 0 {
        return Duration::MIN;
    }

    quiet_period
        .checked_sub(Duration::from_millis(u64::from(now_ms.wrapping_sub(last_activity_ms))))
        .unwrap_or(Duration::MIN)
}

/// Return the time remaining before pointing has been idle for `quiet_period`.
///
/// Qube's display uses this to defer SPI rendering while relative motion is
/// arriving; it does not alter the connection cadence or sleep policy.
pub fn pointing_quiet_period_remaining(quiet_period: Duration) -> Duration {
    quiet_period_remaining(
        Instant::now().as_millis() as u32,
        LAST_POINTING_ACTIVITY_MS.load(Ordering::Acquire),
        quiet_period,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::PointingEvent;

    #[test]
    fn uncommitted_peer_is_forgotten_after_first_failure() {
        let mut retry = PeerRetryState::default();

        assert_eq!(retry.on_failed_attempt(true), FailedPeerAction::Forget);
    }

    #[test]
    fn validated_peer_is_retried_twice_then_forgotten() {
        let mut retry = PeerRetryState::default();

        assert_eq!(
            retry.on_failed_attempt(false),
            FailedPeerAction::Retain {
                consecutive_failures: 1
            }
        );
        assert_eq!(
            retry.on_failed_attempt(false),
            FailedPeerAction::Retain {
                consecutive_failures: 2
            }
        );
        assert_eq!(retry.on_failed_attempt(false), FailedPeerAction::Forget);
    }

    #[test]
    fn successful_validation_resets_peer_failure_count() {
        let mut retry = PeerRetryState::default();
        assert!(matches!(
            retry.on_failed_attempt(false),
            FailedPeerAction::Retain {
                consecutive_failures: 1
            }
        ));
        assert!(matches!(
            retry.on_failed_attempt(false),
            FailedPeerAction::Retain {
                consecutive_failures: 2
            }
        ));

        retry.on_validated();

        assert_eq!(
            retry.on_failed_attempt(false),
            FailedPeerAction::Retain {
                consecutive_failures: 1
            }
        );
    }

    type Ended = Channel<NoopRawMutex, usize, { crate::SPLIT_PERIPHERALS_NUM }>;

    /// How long `after_attempt` holds off the next attempt, by the mock clock.
    async fn held_off(reconnect: &mut SleepReconnect, connected: bool, ended: &Ended) -> Duration {
        let start = Instant::now();
        reconnect.after_attempt(connected, ended).await;
        Instant::now() - start
    }

    fn set_asleep(asleep: bool) {
        crate::ble::sleep::SLEEPING_STATE.store(asleep, Ordering::Release);
    }

    /// Every failed attempt while asleep is followed by another, never by a wait
    /// for a wake-up only the dropped peripheral could cause. The waits double
    /// and stop growing at `SLEEP_RECONNECT_MAX`.
    #[test]
    fn sleeping_central_retries_a_failed_connect_with_bounded_backoff() {
        set_asleep(true);
        let ended = Ended::new();
        let mut reconnect = SleepReconnect::default();
        crate::test_support::test_block_on(async {
            for secs in [1, 2, 4, 8] {
                let waited = held_off(&mut reconnect, false, &ended).await;
                assert!(waited >= Duration::from_secs(secs), "{waited:?} < {secs} s");
                assert!(waited < Duration::from_secs(secs) + Duration::from_millis(10));
            }
            reconnect.failures = 9;
            let waited = held_off(&mut reconnect, false, &ended).await;
            assert!(waited >= SLEEP_RECONNECT_MAX, "{waited:?}");
            assert!(waited < SLEEP_RECONNECT_MAX + Duration::from_millis(10));
        });
    }

    /// A link that reconnects in the middle of a failing streak restarts the
    /// backoff from its first step.
    #[test]
    fn reconnect_midway_restarts_the_backoff() {
        set_asleep(true);
        let ended = Ended::new();
        let mut reconnect = SleepReconnect::default();
        crate::test_support::test_block_on(async {
            held_off(&mut reconnect, false, &ended).await;
            held_off(&mut reconnect, false, &ended).await;
            assert_eq!(held_off(&mut reconnect, true, &ended).await, Duration::from_ticks(0));
            let waited = held_off(&mut reconnect, false, &ended).await;
            assert!(waited < SLEEP_RECONNECT_FIRST + Duration::from_millis(10));
        });
    }

    /// A wake-up or another dropped link ends the wait early: either changes what
    /// the next attempt should do.
    #[test]
    fn backoff_ends_on_wake_or_another_dropped_link() {
        set_asleep(true);
        let ended = Ended::new();
        let mut reconnect = SleepReconnect {
            failures: 3, // an 8 s wait
        };
        crate::test_support::test_block_on(async {
            let drop = async {
                Timer::after_secs(2).await;
                ended.try_send(0).unwrap();
            };
            let (waited, ()) = embassy_futures::join::join(held_off(&mut reconnect, false, &ended), drop).await;
            assert!(waited < Duration::from_secs(3), "{waited:?}");
            ended.try_receive().unwrap();

            let wake = async {
                Timer::after_secs(3).await;
                set_asleep(false);
            };
            let (waited, ()) = embassy_futures::join::join(held_off(&mut reconnect, false, &ended), wake).await;
            assert!(waited < Duration::from_secs(5), "{waited:?}");
        });
    }

    /// Awake there is nothing to back off: a failed attempt is retried at once and
    /// drops a streak left from sleep, so the first failure after falling asleep
    /// again waits only the first step.
    #[test]
    fn awake_central_retries_at_once() {
        set_asleep(false);
        let ended = Ended::new();
        let mut reconnect = SleepReconnect { failures: 4 };
        crate::test_support::test_block_on(async {
            assert_eq!(held_off(&mut reconnect, false, &ended).await, Duration::from_ticks(0));
            set_asleep(true);
            let waited = held_off(&mut reconnect, false, &ended).await;
            assert!(waited < SLEEP_RECONNECT_FIRST + Duration::from_millis(10));
        });
    }

    fn legacy_advertisement(product_id: u16, peripheral_id: u8) -> [u8; 28] {
        let mut data = [0u8; 28];
        data[0..3].copy_from_slice(&[2, 0x01, LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED]);
        data[3] = 17;
        data[4] = 0x07;
        data[5..21].copy_from_slice(&SPLIT_SERVICE_UUID.to_le_bytes());
        data[21..28].copy_from_slice(&[
            6,
            0xff,
            (LEGACY_SPLIT_COMPANY_ID & 0xff) as u8,
            (LEGACY_SPLIT_COMPANY_ID >> 8) as u8,
            (product_id & 0xff) as u8,
            (product_id >> 8) as u8,
            peripheral_id,
        ]);
        data
    }

    #[test]
    fn legacy_advertisement_requires_matching_product() {
        let matching = legacy_advertisement(crate::SPLIT_PRODUCT_ID, 1);
        assert_eq!(split_peripheral_id_from_advertisement(&matching), Some(1));

        let mismatched = legacy_advertisement(crate::SPLIT_PRODUCT_ID.wrapping_add(1), 1);
        assert_eq!(split_peripheral_id_from_advertisement(&mismatched), None);
    }

    #[test]
    fn legacy_advertisement_requires_split_service() {
        let mut data = legacy_advertisement(crate::SPLIT_PRODUCT_ID, 0);
        data[4] = 0x06;
        assert_eq!(split_peripheral_id_from_advertisement(&data), None);
    }

    fn rmk_advertisement(product_id: u16, peripheral_id: u8) -> [u8; 11] {
        let [lo, hi] = product_id.to_le_bytes();
        [
            2,
            0x01,
            BR_EDR_NOT_SUPPORTED,
            7,
            0xff,
            0x53,
            0x52,
            0,
            peripheral_id,
            lo,
            hi,
        ]
    }

    // The scan took any RMK split peripheral, and only the product check after
    // connecting turned another keyboard's half away. Pairing after a reset
    // kept landing on that half and never reached this keyboard's own.
    #[test]
    fn advertisement_of_another_keyboard_is_skipped_while_scanning() {
        let own = rmk_advertisement(crate::SPLIT_PRODUCT_ID, 1);
        assert_eq!(split_peripheral_id_from_advertisement(&own), Some(1));

        let foreign = rmk_advertisement(crate::SPLIT_PRODUCT_ID.wrapping_add(1), 1);
        assert_eq!(split_peripheral_id_from_advertisement(&foreign), None);
    }

    #[test]
    fn pointing_profile_uses_7_5_ms_without_slave_latency() {
        let params = active_central_conn_param(SplitLinkProfile::Pointing);

        assert_eq!(params.min_connection_interval, Duration::from_micros(7_500));
        assert_eq!(params.max_connection_interval, Duration::from_micros(7_500));
        assert_eq!(params.max_latency, 0);
    }

    #[test]
    fn keyboard_profile_retains_15_ms_cadence() {
        let params = active_central_conn_param(SplitLinkProfile::Keyboard);

        assert_eq!(params.min_connection_interval, Duration::from_millis(15));
        assert_eq!(params.max_connection_interval, Duration::from_millis(15));
        assert_eq!(params.max_latency, 0);
    }

    #[test]
    fn runtime_profile_is_set_per_link() {
        assert_eq!(split_link_profile(30), SplitLinkProfile::Keyboard);
        assert!(set_split_link_profile(30, SplitLinkProfile::Pointing));
        assert_eq!(split_link_profile(30), SplitLinkProfile::Pointing);
        assert_eq!(split_link_profile(27), SplitLinkProfile::Keyboard);
        assert!(set_split_link_profile(30, SplitLinkProfile::Keyboard));
        assert_eq!(split_link_profile(30), SplitLinkProfile::Keyboard);
        assert!(!set_split_link_profile(32, SplitLinkProfile::Pointing));
    }

    #[test]
    fn pointing_link_relaxes_to_keyboard_cadence_after_the_motion_hold() {
        let hold_ms = POINTING_LINK_HOLD.as_millis() as u32;
        assert!(set_split_link_profile(29, SplitLinkProfile::Pointing));

        // No motion since boot: nothing to hold.
        assert_eq!(target_split_link_profile(29, 5_000), SplitLinkProfile::Keyboard);

        note_link_pointing_activity(29, 10_000);
        assert_eq!(target_split_link_profile(29, 10_000), SplitLinkProfile::Pointing);
        assert_eq!(
            target_split_link_profile(29, 10_000 + hold_ms - 1),
            SplitLinkProfile::Pointing
        );
        assert_eq!(
            target_split_link_profile(29, 10_000 + hold_ms),
            SplitLinkProfile::Keyboard
        );

        // A key-only link never runs the pointing cadence, however recent the motion.
        assert!(set_split_link_profile(29, SplitLinkProfile::Keyboard));
        assert_eq!(target_split_link_profile(29, 10_000), SplitLinkProfile::Keyboard);
        assert!(set_split_link_profile(29, SplitLinkProfile::Pointing));
        assert_eq!(target_split_link_profile(29, 10_000), SplitLinkProfile::Pointing);
    }

    #[test]
    fn relaxed_pointing_link_is_woken_by_the_next_motion_only() {
        let wake = &LINK_POINTING_WAKE[28];
        wake.reset();
        assert!(set_split_link_profile(28, SplitLinkProfile::Pointing));

        // Live at the pointing cadence: motion just refreshes the hold.
        set_applied_split_link_profile(28, SplitLinkProfile::Pointing);
        note_link_pointing_activity(28, 1_000);
        assert!(!wake.signaled());

        // Relaxed: the first motion must wake the follower.
        set_applied_split_link_profile(28, SplitLinkProfile::Keyboard);
        note_link_pointing_activity(28, 2_000);
        assert!(wake.signaled());
        wake.reset();

        // A link configured key-only is not "relaxed", so motion stays silent.
        assert!(set_split_link_profile(28, SplitLinkProfile::Keyboard));
        set_applied_split_link_profile(28, SplitLinkProfile::Keyboard);
        note_link_pointing_activity(28, 3_000);
        assert!(!wake.signaled());
    }

    #[test]
    fn pointing_quiet_period_waits_only_for_recent_motion() {
        let quiet_period = Duration::from_millis(100);

        assert_eq!(quiet_period_remaining(1_000, 0, quiet_period), Duration::MIN);
        assert_eq!(
            quiet_period_remaining(1_050, 1_000, quiet_period),
            Duration::from_millis(50)
        );
        assert_eq!(quiet_period_remaining(1_100, 1_000, quiet_period), Duration::MIN);
    }

    #[test]
    fn pointing_activity_threshold_ignores_pmw3610_settling_noise() {
        use crate::event::{Axis, AxisEvent, AxisValType};

        let event = |x, y| PointingEvent {
            device_id: 0,
            axes: [
                AxisEvent {
                    typ: AxisValType::Rel,
                    axis: Axis::X,
                    value: x,
                },
                AxisEvent {
                    typ: AxisValType::Rel,
                    axis: Axis::Y,
                    value: y,
                },
                AxisEvent {
                    typ: AxisValType::Rel,
                    axis: Axis::Z,
                    value: 0,
                },
            ],
        };

        assert!(!event(1, -1).has_relative_xy_motion(POINTING_ACTIVITY_THRESHOLD));
        assert!(event(2, 0).has_relative_xy_motion(POINTING_ACTIVITY_THRESHOLD));
    }

    #[test]
    fn pointing_quiet_period_handles_millisecond_counter_wrap() {
        let quiet_period = Duration::from_millis(100);
        let last = u32::MAX - 20;

        assert_eq!(
            quiet_period_remaining(29, last, quiet_period),
            Duration::from_millis(50)
        );
    }

    #[cfg(not(feature = "subrating"))]
    #[test]
    fn sleeping_split_link_keeps_short_burst_interval() {
        for (usb_powered, interval, latency) in [(false, 30, 6), (true, 15, 13)] {
            let params = sleeping_central_conn_param_for(SplitLinkProfile::Keyboard, usb_powered);

            assert_eq!(params.min_connection_interval, Duration::from_millis(interval));
            assert_eq!(params.max_connection_interval, Duration::from_millis(interval));
            assert_eq!(params.max_latency, latency);
            assert_eq!(
                params.max_connection_interval.as_millis() * (u64::from(params.max_latency) + 1),
                210
            );
        }
    }

    #[cfg(not(feature = "subrating"))]
    #[test]
    fn usb_powered_central_sleeps_every_link_on_the_short_anchor() {
        for profile in [SplitLinkProfile::Keyboard, SplitLinkProfile::Pointing] {
            let params = sleeping_central_conn_param_for(profile, true);

            assert_eq!(params.max_connection_interval, Duration::from_millis(15));
            assert_eq!(params.max_latency, 13);
        }
    }

    #[cfg(all(not(feature = "subrating"), not(feature = "host_first_split_wake")))]
    #[test]
    fn pointing_sleep_profile_is_unchanged_without_opt_in() {
        let params = sleeping_central_conn_param_for(SplitLinkProfile::Pointing, false);

        assert_eq!(params.min_connection_interval, Duration::from_millis(30));
        assert_eq!(params.max_connection_interval, Duration::from_millis(30));
        assert_eq!(params.max_latency, 6);
    }

    #[cfg(all(not(feature = "subrating"), feature = "host_first_split_wake"))]
    #[test]
    fn sleeping_pointing_link_uses_15_ms_anchor_and_210ms_cadence() {
        let params = sleeping_central_conn_param_for(SplitLinkProfile::Pointing, false);

        assert!(params.is_valid());
        assert_eq!(params.min_connection_interval, Duration::from_millis(15));
        assert_eq!(params.max_connection_interval, Duration::from_millis(15));
        assert_eq!(params.max_latency, 13);
        assert_eq!(
            params.max_connection_interval.as_micros() * (u64::from(params.max_latency) + 1),
            210_000
        );
    }

    #[cfg(all(not(feature = "subrating"), feature = "host_first_split_wake"))]
    #[test]
    fn pointing_wake_waits_only_while_host_gate_is_pending() {
        assert_eq!(
            split_wake_order_action(SplitLinkProfile::Pointing, true, HostWakeOrderGate::Pending),
            SplitWakeOrderAction::WaitForHost
        );
        assert_eq!(
            split_wake_order_action(SplitLinkProfile::Pointing, true, HostWakeOrderGate::Open),
            SplitWakeOrderAction::Proceed
        );
        assert_eq!(
            split_wake_order_action(SplitLinkProfile::Keyboard, true, HostWakeOrderGate::Pending),
            SplitWakeOrderAction::Proceed
        );
    }

    #[cfg(all(not(feature = "subrating"), feature = "host_first_split_wake"))]
    #[test]
    fn wake_revalidation_drops_stale_active_target_after_resleep() {
        assert_eq!(
            split_link_update_plan(SplitLinkProfile::Pointing, true, SplitLinkProfile::Pointing, false),
            SplitLinkUpdatePlan::Active(SplitLinkProfile::Pointing)
        );
        assert_eq!(
            split_link_update_plan(SplitLinkProfile::Pointing, true, SplitLinkProfile::Pointing, true),
            SplitLinkUpdatePlan::None
        );
    }

    #[cfg(all(not(feature = "subrating"), feature = "host_first_split_wake"))]
    #[test]
    fn wake_revalidation_uses_latest_runtime_profile() {
        assert_eq!(
            split_link_update_plan(SplitLinkProfile::Pointing, true, SplitLinkProfile::Keyboard, false),
            SplitLinkUpdatePlan::Active(SplitLinkProfile::Keyboard)
        );
    }

    #[cfg(all(not(feature = "subrating"), not(feature = "host_first_split_wake")))]
    mod follower {
        use super::*;

        fn at(ms: u64) -> Instant {
            Instant::from_millis(ms)
        }

        const AWAKE_KEYBOARD: SplitLinkTarget = SplitLinkTarget {
            sleeping: false,
            profile: SplitLinkProfile::Keyboard,
        };
        const AWAKE_POINTING: SplitLinkTarget = SplitLinkTarget {
            sleeping: false,
            profile: SplitLinkProfile::Pointing,
        };
        const ASLEEP_KEYBOARD: SplitLinkTarget = SplitLinkTarget {
            sleeping: true,
            profile: SplitLinkProfile::Keyboard,
        };
        const ASLEEP_POINTING: SplitLinkTarget = SplitLinkTarget {
            sleeping: true,
            profile: SplitLinkProfile::Pointing,
        };

        #[test]
        fn live_params_must_match_the_whole_requested_tuple() {
            let params = active_central_conn_param(SplitLinkProfile::Keyboard);
            let live = ConnParams {
                conn_interval: params.max_connection_interval,
                peripheral_latency: params.max_latency,
                supervision_timeout: params.supervision_timeout,
            };
            assert!(split_link_params_live(&live, &params));

            // Sleeping and active tuples on a USB central share the 15 ms interval.
            let sleeping = sleeping_central_conn_param_for(SplitLinkProfile::Keyboard, true);
            assert!(!split_link_params_live(&live, &sleeping));
            assert!(!split_link_params_live(
                &ConnParams {
                    supervision_timeout: Duration::from_secs(11),
                    ..live
                },
                &params
            ));
        }

        #[test]
        fn link_reconnecting_while_asleep_requests_only_the_sleep_cadence() {
            let mut follower = SplitLinkFollower::default();

            // The first request of a new link is the latched sleep target, never
            // an active update that the sleep update would have to overtake.
            assert_eq!(follower.step(ASLEEP_KEYBOARD, at(0)), SplitLinkStep::Apply);
            follower.on_result(ASLEEP_KEYBOARD, true, at(100));
            assert_eq!(follower.step(ASLEEP_KEYBOARD, at(200)), SplitLinkStep::Settled);
            assert_eq!(follower.applied(), Some(ASLEEP_KEYBOARD));
        }

        #[test]
        fn rejected_sleep_update_is_retried_with_backoff_until_confirmed() {
            let mut follower = SplitLinkFollower::default();
            follower.on_result(AWAKE_KEYBOARD, true, at(0));

            assert_eq!(follower.step(ASLEEP_KEYBOARD, at(1_000)), SplitLinkStep::Apply);
            follower.on_result(ASLEEP_KEYBOARD, false, at(3_000));
            assert_eq!(follower.applied(), Some(AWAKE_KEYBOARD));
            assert_eq!(
                follower.step(ASLEEP_KEYBOARD, at(3_400)),
                SplitLinkStep::RetryIn(Duration::from_millis(600))
            );
            assert_eq!(follower.step(ASLEEP_KEYBOARD, at(4_000)), SplitLinkStep::Apply);

            // Consecutive failures double the backoff up to its ceiling.
            let mut now = 4_000;
            for backoff in [2_000, 4_000, 8_000, 8_000] {
                follower.on_result(ASLEEP_KEYBOARD, false, at(now));
                assert_eq!(
                    follower.step(ASLEEP_KEYBOARD, at(now)),
                    SplitLinkStep::RetryIn(Duration::from_millis(backoff))
                );
                now += backoff;
                assert_eq!(follower.step(ASLEEP_KEYBOARD, at(now)), SplitLinkStep::Apply);
            }

            follower.on_result(ASLEEP_KEYBOARD, true, at(now));
            assert_eq!(follower.step(ASLEEP_KEYBOARD, at(now)), SplitLinkStep::Settled);
        }

        #[test]
        fn rejected_wake_update_is_retried_and_never_delays_the_next_transition() {
            let mut follower = SplitLinkFollower::default();
            follower.on_result(ASLEEP_KEYBOARD, true, at(0));

            assert_eq!(follower.step(AWAKE_POINTING, at(1_000)), SplitLinkStep::Apply);
            follower.on_result(AWAKE_POINTING, false, at(1_000));
            assert_eq!(
                follower.step(AWAKE_POINTING, at(1_500)),
                SplitLinkStep::RetryIn(Duration::from_millis(500))
            );
            assert_eq!(follower.step(AWAKE_POINTING, at(2_000)), SplitLinkStep::Apply);

            // The hold ran out meanwhile: the new target goes out at once.
            follower.on_result(AWAKE_POINTING, false, at(2_000));
            assert_eq!(follower.step(AWAKE_KEYBOARD, at(2_100)), SplitLinkStep::Apply);

            // Back to sleep before any wake update landed: nothing to do.
            assert_eq!(follower.step(ASLEEP_KEYBOARD, at(2_200)), SplitLinkStep::Settled);
        }

        #[test]
        fn profile_change_while_asleep_is_reconciled() {
            let mut follower = SplitLinkFollower::default();
            follower.on_result(ASLEEP_KEYBOARD, true, at(0));

            let expected = if ASLEEP_POINTING.conn_params() == ASLEEP_KEYBOARD.conn_params() {
                SplitLinkStep::Settled
            } else {
                SplitLinkStep::Apply
            };
            assert_eq!(follower.step(ASLEEP_POINTING, at(1_000)), expected);
            if expected == SplitLinkStep::Apply {
                follower.on_result(ASLEEP_POINTING, true, at(1_100));
            }
            assert_eq!(follower.applied(), Some(ASLEEP_POINTING));

            // Waking restores the new profile's active cadence.
            assert_eq!(follower.step(AWAKE_POINTING, at(2_000)), SplitLinkStep::Apply);
        }

        #[test]
        fn keyboard_to_pointing_switch_arms_the_first_motion() {
            let wake = &LINK_POINTING_WAKE[26];
            wake.reset();
            let mut follower = SplitLinkFollower::default();
            follower.on_result(AWAKE_KEYBOARD, true, at(0));
            note_split_link_target(26, AWAKE_KEYBOARD);

            // Switched to Pointing with no recent motion: the link stays at the
            // keyboard cadence, but must be registered as a relaxed pointing link.
            assert!(set_split_link_profile(26, SplitLinkProfile::Pointing));
            let target = split_link_target(26, false, 60_000);
            assert_eq!(target, AWAKE_KEYBOARD);
            assert_eq!(follower.step(target, at(60_000)), SplitLinkStep::Settled);
            note_split_link_target(26, target);

            note_link_pointing_activity(26, 61_000);
            assert!(wake.signaled());
            let target = split_link_target(26, false, 61_000);
            assert_eq!(target, AWAKE_POINTING);
            assert_eq!(follower.step(target, at(61_000)), SplitLinkStep::Apply);
            wake.reset();
        }
    }

    #[test]
    fn split_scan_leaves_radio_time_for_established_links() {
        let config = scan_config(SPLIT_CENTRAL_SCAN_WINDOW);

        assert!(!config.active);
        assert_eq!(config.interval, Duration::from_millis(100));
        assert_eq!(config.window, Duration::from_millis(30));
    }
}
