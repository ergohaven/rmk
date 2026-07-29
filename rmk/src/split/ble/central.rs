use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use bt_hci::cmd::le::{LeReadLocalSupportedFeatures, LeSetPhy, LeSetScanParams};
use bt_hci::controller::{ControllerCmdAsync, ControllerCmdSync};
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_sync::watch::{Receiver, Watch};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use heapless::VecView;
use trouble_host::prelude::*;

use crate::ble::{SLEEPING_STATE, update_ble_phy, update_conn_params};
use crate::channel::FLASH_CHANNEL;
use crate::event::{
    PeripheralConnectedEvent, SleepStateEvent, SplitConnectionState, SplitConnectionStateEvent, publish_event,
};
#[cfg(feature = "storage")]
use crate::split::ble::PeerAddress;
use crate::split::driver::{PeripheralManager, SplitDriverError, SplitReader, SplitWriter};
use crate::split::{SPLIT_MESSAGE_MAX_SIZE, SplitMessage};
use crate::storage::FlashOperationMessage;
use crate::{SPLIT_CENTRAL_SLEEP_TIMEOUT_SECONDS, SPLIT_PAIRING_TIMEOUT_SECONDS};

pub(crate) static STACK_STARTED: Signal<crate::RawMutex, bool> = Signal::new();
pub(crate) static PERIPHERAL_FOUND: Signal<crate::RawMutex, (u8, BdAddr)> = Signal::new();

// Signals and mutex for syncing scanning state between scanning task and peripheral manager
static START_SCANNING: Signal<crate::RawMutex, ()> = Signal::new();
static STOP_SCANNING: Signal<crate::RawMutex, ()> = Signal::new();
static SCANNING_MUTEX: Mutex<crate::RawMutex, ()> = Mutex::new(());
static UNCOMMITTED_PEER_CANDIDATES: BlockingMutex<crate::RawMutex, Cell<u32>> = BlockingMutex::new(Cell::new(0));
static CONNECTED_PERIPHERALS: AtomicU32 = AtomicU32::new(0);
static PERIPHERAL_CONNECTION_CHANGED: Signal<crate::RawMutex, ()> = Signal::new();
static SPLIT_WINDOW_RESTART: Signal<crate::RawMutex, u32> = Signal::new();
static SPLIT_WINDOW_DONE: Signal<crate::RawMutex, u32> = Signal::new();
static SPLIT_WINDOW_GENERATION: AtomicU32 = AtomicU32::new(0);

static LAST_ACTIVITY_MS: AtomicU32 = AtomicU32::new(0);
static LAST_POINTING_ACTIVITY_MS: AtomicU32 = AtomicU32::new(0);
static SPLIT_SLEEP_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Notifies every power-mode watcher that activity was just recorded, so waking
/// a sleeping link doesn't wait out a poll tick. `Watch` rather than `Signal`
/// because each split link has its own manager and none may consume another's
/// wake-up.
static SPLIT_ACTIVITY_WATCH: Watch<crate::RawMutex, u32, SPLIT_POWER_WATCHERS> = Watch::new();

const SPLIT_POINTING_ACTIVE_WINDOW_MS: u32 = 500;
const SPLIT_ACTIVE_WINDOW_MS: u32 = 2_000;
const SPLIT_POWER_POLL_MS: u64 = 100;
/// One watcher per split link, plus the central's own power state manager.
const SPLIT_POWER_WATCHERS: usize = crate::SPLIT_PERIPHERALS_NUM + 1;

const SPLIT_SERVICE_UUID: [u8; 16] = [70, 153, 101, 152, 54, 53, 10, 191, 7, 75, 229, 24, 170, 251, 213, 77];
const SPLIT_COMPANY_ID: u16 = 0xe118;

fn required_peripheral_mask() -> u32 {
    if crate::SPLIT_PERIPHERALS_NUM >= u32::BITS as usize {
        u32::MAX
    } else {
        (1u32 << crate::SPLIT_PERIPHERALS_NUM) - 1
    }
}

fn all_peripherals_connected() -> bool {
    CONNECTED_PERIPHERALS.load(Ordering::Acquire) & required_peripheral_mask() == required_peripheral_mask()
}

fn publish_peripheral_connection(id: usize, connected: bool) {
    let bit = bit_for_peri(id);
    if connected {
        CONNECTED_PERIPHERALS.fetch_or(bit, Ordering::AcqRel);
    } else {
        CONNECTED_PERIPHERALS.fetch_and(!bit, Ordering::AcqRel);
    }
    publish_event(PeripheralConnectedEvent { id, connected });
    PERIPHERAL_CONNECTION_CHANGED.signal(());
}

fn publish_split_connection_state(state: SplitConnectionState, generation: u32, terminal: bool) {
    publish_event(SplitConnectionStateEvent(state));
    if terminal {
        SPLIT_WINDOW_DONE.signal(generation);
    }
}

/// Supervise the complete split-link search window.
///
/// Peripheral managers keep reconnecting in the background, while this task
/// owns the visible `Searching -> Connected/Idle` state and its timeout.
pub async fn run_split_connection_supervisor() {
    let timeout = Duration::from_secs(u64::from(SPLIT_PAIRING_TIMEOUT_SECONDS));
    let mut generation = SPLIT_WINDOW_GENERATION.load(Ordering::Acquire);
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

    let generation = SPLIT_WINDOW_GENERATION.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
    SPLIT_WINDOW_RESTART.signal(generation);
    loop {
        if SPLIT_WINDOW_DONE.wait().await == generation {
            return;
        }
    }
}

/// Gatt service used in split central to send split message to peripheral
#[gatt_service(uuid = "4dd5fbaa-18e5-4b07-bf0a-353698659946")]
struct SplitBleCentralService {
    #[characteristic(uuid = "0e6313e3-bd0b-45c2-8d2e-37a2e8128bc3", read, notify)]
    message_to_central: [u8; SPLIT_MESSAGE_MAX_SIZE],

    #[characteristic(uuid = "4b3514fb-cae4-4d38-a097-3a2a3d1c3b9c", write_without_response, read, notify)]
    message_to_peripheral: [u8; SPLIT_MESSAGE_MAX_SIZE],
}

/// Gatt server in split peripheral
#[gatt_server]
struct BleSplitCentralServer {
    service: SplitBleCentralService,
}

pub async fn scan_peripherals<
    'b,
    's: 'b,
    C: Controller
        + ControllerCmdSync<LeSetScanParams>
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>,
>(
    stack: &'b Stack<'s, C, DefaultPacketPool>,
    addrs: &RefCell<VecView<Option<[u8; 6]>>>,
) {
    loop {
        // Wait unitil `START_SCANNING` is signaled
        START_SCANNING.wait().await;
        // Check whether the scanning is needed, aka there's empty slot in the addr list.
        let need_scan = !addrs.borrow().iter().all(|a| a.is_some());
        if need_scan {
            let scanning_fut = async {
                loop {
                    let mut central = stack.central();
                    wait_for_stack_started().await;
                    let mut scanner = Scanner::new(&mut central);
                    let scan_config = ScanConfig {
                        active: false,
                        ..Default::default()
                    };
                    let _guard = SCANNING_MUTEX.lock().await;
                    if let Ok(_session) = scanner.scan(&scan_config).await {
                        info!("Start scanning peripherals");
                        STOP_SCANNING.wait().await;
                        info!("Stop scanning");
                    }
                }
            };
            let update_addrs_fut = async {
                loop {
                    let (found_peripheral_id, addr) = PERIPHERAL_FOUND.wait().await;
                    let scanned_addr = addr.into_inner();
                    if let Some(Some(stored_addr)) = addrs.borrow_mut().get_mut(found_peripheral_id as usize)
                        && *stored_addr == scanned_addr
                    {
                        continue;
                    }

                    info!("Scanned split peripheral {:?}", scanned_addr);
                    let mut slot_updated = false;
                    if let Some(slot) = addrs.borrow_mut().get_mut(found_peripheral_id as usize)
                        && slot.is_none()
                    {
                        *slot = Some(scanned_addr);
                        slot_updated = true;
                    }

                    // Do not persist a scanned address until the GATT product-id
                    // handshake proves that it belongs to this keyboard model.
                    if slot_updated {
                        mark_uncommitted_peer_candidate(found_peripheral_id as usize);
                    }

                    if addrs.borrow().iter().all(|a| a.is_some()) {
                        break;
                    }
                }
            };

            // Scan until all peripherals are scanned
            // TODO: Timeout?
            select(scanning_fut, update_addrs_fut).await;
        }
    }
}

// When no peripheral address is saved, the central should first scan for peripheral.
// This handler is used to handle the scan result.
pub(crate) struct ScanHandler {}

impl EventHandler for ScanHandler {
    fn on_adv_reports(&self, mut it: LeAdvReportsIter<'_>) {
        while let Some(Ok(report)) = it.next() {
            if let Some(peripheral_id) = split_peripheral_id_from_advertisement(report.data)
                .or_else(|| legacy_split_peripheral_id_from_advertisement(report.data))
            {
                info!("Found split peripheral: id={:?}, addr={:?}", peripheral_id, report.addr);
                PERIPHERAL_FOUND.signal((peripheral_id, report.addr));
                break;
            }
        }
    }
}

// Migration compatibility for the previous upstream/Qube advertisement,
// which carried only the peripheral id. Product identity is still verified
// by the GATT handshake before the address is persisted.
fn legacy_split_peripheral_id_from_advertisement(data: &[u8]) -> Option<u8> {
    if data.len() > 25
        && data[4] == 0x07
        && data[5..].starts_with(&SPLIT_SERVICE_UUID)
        && data[21..25] == [0x04, 0xff, 0x18, 0xe1]
    {
        Some(data[25])
    } else {
        None
    }
}

fn split_peripheral_id_from_advertisement(data: &[u8]) -> Option<u8> {
    let mut has_split_service = false;
    let mut matching_product_peripheral_id = None;
    let mut offset = 0usize;

    while offset < data.len() {
        let len = data[offset] as usize;
        if len == 0 {
            break;
        }
        let end = offset + 1 + len;
        if end > data.len() || len < 1 {
            break;
        }

        let ad_type = data[offset + 1];
        let payload = &data[offset + 2..end];
        match ad_type {
            0x07 if payload == SPLIT_SERVICE_UUID => {
                has_split_service = true;
            }
            0xff if payload.len() >= 5 => {
                let company_id = u16::from_le_bytes([payload[0], payload[1]]);
                let product_id = u16::from_le_bytes([payload[2], payload[3]]);
                if company_id == SPLIT_COMPANY_ID && product_id == crate::SPLIT_PRODUCT_ID {
                    matching_product_peripheral_id = Some(payload[4]);
                }
            }
            _ => {}
        }

        offset = end;
    }

    has_split_service.then_some(matching_product_peripheral_id).flatten()
}

fn bit_for_peri(peri_id: usize) -> u32 {
    1u32 << peri_id.min(31)
}

fn mark_uncommitted_peer_candidate(peri_id: usize) {
    let bit = bit_for_peri(peri_id);
    UNCOMMITTED_PEER_CANDIDATES.lock(|cell| cell.set(cell.get() | bit));
}

fn take_uncommitted_peer_candidate(peri_id: usize) -> bool {
    let bit = bit_for_peri(peri_id);
    UNCOMMITTED_PEER_CANDIDATES.lock(|cell| {
        let current = cell.get();
        cell.set(current & !bit);
        current & bit != 0
    })
}

async fn forget_failed_peer(peri_id: usize, addrs: &RefCell<VecView<Option<[u8; 6]>>>) {
    take_uncommitted_peer_candidate(peri_id);
    if let Some(addr) = addrs.borrow_mut().get_mut(peri_id) {
        *addr = None;
    }

    // A stored address can belong to an older Qube/half pairing. Keeping it
    // after an initiating failure prevents the central from ever scanning for
    // the currently powered peripheral. Invalidate only after the connection
    // attempt itself fails; normal disconnects retain the proven address.
    #[cfg(feature = "storage")]
    FLASH_CHANNEL
        .send(FlashOperationMessage::PeerAddress(PeerAddress::new(
            peri_id as u8,
            false,
            [0; 6],
        )))
        .await;
}

async fn commit_peer_candidate(peri_id: usize, peer_address: [u8; 6]) {
    if !take_uncommitted_peer_candidate(peri_id) {
        return;
    }

    #[cfg(feature = "storage")]
    FLASH_CHANNEL
        .send(FlashOperationMessage::PeerAddress(PeerAddress::new(
            peri_id as u8,
            true,
            peer_address,
        )))
        .await;
}

pub(crate) async fn run_ble_peripheral_manager<
    'b,
    's: 'b,
    C: Controller
        + ControllerCmdSync<LeSetScanParams>
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>,
    const ROW: usize,
    const COL: usize,
    const ROW_OFFSET: usize,
    const COL_OFFSET: usize,
>(
    peri_id: usize,
    addrs: &RefCell<VecView<Option<[u8; 6]>>>,
    stack: &'b Stack<'s, C, DefaultPacketPool>,
) {
    trace!("SPLIT_MESSAGE_MAX_SIZE: {}", SPLIT_MESSAGE_MAX_SIZE);

    loop {
        // Check until the address is available
        let address = loop {
            if let Some(Some(addr)) = addrs.borrow().get(peri_id) {
                break Address::random(*addr);
            }
            if !START_SCANNING.signaled() {
                START_SCANNING.signal(());
            }
            // Check again after 500ms
            embassy_time::Timer::after_millis(500).await;
        };
        info!("Peripheral peer address: {:?}", address);

        let mut central = stack.central();
        let config = ConnectConfig {
            connect_params: defaul_central_conn_param(),
            scan_config: ScanConfig {
                filter_accept_list: &[address],
                // Match the effective 62.5 ms initiating scan used by the
                // last working bt-hci 0.6 firmware.
                interval: Duration::from_micros(62_500),
                window: Duration::from_micros(62_500),
                ..Default::default()
            },
        };
        wait_for_stack_started().await;

        publish_peripheral_connection(peri_id, false);

        // Connect to peripheral
        match with_timeout(Duration::from_secs(5), async {
            if let Ok(_guard) = SCANNING_MUTEX.try_lock() {
                info!("Start connecting to peripheral {}", peri_id);
                central.connect(&config).await
            } else {
                STOP_SCANNING.signal(());
                let _guard = SCANNING_MUTEX.lock().await;
                // Wait a little bit to ensure that the scanning has been fully stopped
                embassy_time::Timer::after_millis(100).await;
                info!("Start connecting to peripheral {}", peri_id);
                central.connect(&config).await
            }
        })
        .await
        {
            Ok(Ok(conn)) => {
                info!("Connected to peripheral {}", peri_id);
                let peer_validated = Cell::new(false);

                if let Err(e) = run_central_manager_task::<_, _, ROW, COL, ROW_OFFSET, COL_OFFSET>(
                    peri_id,
                    address.addr.into_inner(),
                    stack,
                    &conn,
                    &peer_validated,
                )
                .await
                {
                    #[cfg(feature = "defmt")]
                    let e = defmt::Debug2Format(&e);
                    error!("BLE central error: {:?}", e);
                }
                if !peer_validated.get() {
                    warn!("Split peripheral {} disconnected before validation", peri_id);
                    // A successful HCI connection can still fail during GATT
                    // discovery or product validation. Treat that exactly like
                    // an initiating failure so a stale saved address cannot
                    // trap Qube in an endless reconnect loop.
                    forget_failed_peer(peri_id, addrs).await;
                }
            }
            Ok(Err(e)) => {
                #[cfg(feature = "defmt")]
                let e = defmt::Debug2Format(&e);
                error!("Connect to peripheral {} error: {:?}", peri_id, e);
                forget_failed_peer(peri_id, addrs).await;
            }
            Err(_) => {
                warn!("Connect to peripheral {} timeout", peri_id);
                forget_failed_peer(peri_id, addrs).await;
            }
        }
        // Reconnect after 500ms
        embassy_time::Timer::after_millis(500).await;
    }
}

fn defaul_central_conn_param() -> RequestedConnParams {
    RequestedConnParams {
        min_connection_interval: Duration::from_millis(15),
        max_connection_interval: Duration::from_millis(15),
        // Keep active split links awake every interval so central-to-peripheral
        // layer/state updates reach LEDs without slave-latency delay.
        max_latency: 0,
        supervision_timeout: Duration::from_secs(5),
        ..Default::default()
    }
}

fn pointing_central_conn_param() -> RequestedConnParams {
    RequestedConnParams {
        min_connection_interval: Duration::from_micros(7_500),
        max_connection_interval: Duration::from_micros(7_500),
        // Pointing reports are latency-sensitive and arrive continuously, so
        // keep the peripheral present at every connection event.
        max_latency: 0,
        supervision_timeout: Duration::from_secs(5),
        ..Default::default()
    }
}

fn idle_central_conn_param() -> RequestedConnParams {
    RequestedConnParams {
        min_connection_interval: Duration::from_millis(30),
        max_connection_interval: Duration::from_millis(30),
        max_latency: 0,
        supervision_timeout: Duration::from_secs(5),
        ..Default::default()
    }
}

fn sleeping_central_conn_param() -> RequestedConnParams {
    RequestedConnParams {
        // The interval bounds both the first packet after a long idle and the
        // ramp back to the active parameters, since a connection update takes
        // effect only after several connection events. At 200ms a touchpad on
        // the peripheral half stuttered for over a second before smoothing out.
        min_connection_interval: Duration::from_millis(100),
        max_connection_interval: Duration::from_millis(100),
        // A sleeping peripheral must still attend every connection event.
        // Allowing 25 skipped events delayed the first key or pointing packet
        // from a split peripheral by up to five seconds.
        max_latency: 0,
        supervision_timeout: Duration::from_secs(11),
        ..Default::default()
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

async fn run_central_manager_task<
    'b,
    's: 'b,
    C: Controller + ControllerCmdAsync<LeSetPhy> + ControllerCmdSync<LeReadLocalSupportedFeatures>,
    P: PacketPool,
    const ROW: usize,
    const COL: usize,
    const ROW_OFFSET: usize,
    const COL_OFFSET: usize,
>(
    id: usize,
    peer_address: [u8; 6],
    stack: &'b Stack<'s, C, P>,
    conn: &Connection<'b, P>,
    peer_validated: &Cell<bool>,
) -> Result<(), BleHostError<C::Error>> {
    let client = GattClient::<C, P, 10>::new(stack, conn).await?;

    // Use 2M Phy
    update_ble_phy(stack, conn).await;

    info!("Updating connection parameters for peripheral");
    update_conn_params(stack, conn, &defaul_central_conn_param()).await;

    let result = match select3(
        ble_central_task(&client, conn),
        run_peripheral_manager::<_, _, ROW, COL, ROW_OFFSET, COL_OFFSET>(id, peer_address, &client, peer_validated),
        sleep_manager_task(stack, conn),
    )
    .await
    {
        Either3::First(e) => e,
        Either3::Second(e) => e,
        Either3::Third(e) => e,
    };

    result
}

async fn ble_central_task<'a, C: Controller + ControllerCmdAsync<LeSetPhy>, P: PacketPool>(
    client: &GattClient<'a, C, P, 10>,
    conn: &Connection<'a, P>,
) -> Result<(), BleHostError<C::Error>> {
    // Simply monitor connection status
    let conn_check = async {
        while conn.is_connected() {
            Timer::after_secs(5).await;
        }
    };

    match select(client.task(), conn_check).await {
        Either::First(e) => e,
        Either::Second(_) => {
            info!("Connection lost");
            Ok(())
        }
    }
}

async fn run_peripheral_manager<
    'a,
    C: Controller + ControllerCmdAsync<LeSetPhy>,
    P: PacketPool,
    const ROW: usize,
    const COL: usize,
    const ROW_OFFSET: usize,
    const COL_OFFSET: usize,
>(
    id: usize,
    peer_address: [u8; 6],
    client: &GattClient<'a, C, P, 10>,
    peer_validated: &Cell<bool>,
) -> Result<(), BleHostError<C::Error>> {
    let services = client.services_by_uuid(&Uuid::new_long(SPLIT_SERVICE_UUID)).await?;
    info!("Services found");
    if let Some(service) = services.first() {
        let message_to_central = client
            .characteristic_by_uuid::<[u8; SPLIT_MESSAGE_MAX_SIZE]>(
                service,
                // uuid: 0e6313e3-bd0b-45c2-8d2e-37a2e8128bc3
                &Uuid::Uuid128([
                    195u8, 139u8, 18u8, 232u8, 162u8, 55u8, 46u8, 141u8, 194u8, 69u8, 11u8, 189u8, 227u8, 19u8, 99u8,
                    14u8,
                ]),
            )
            .await?;
        info!("Message to central found");
        let message_to_peripheral = client
            .characteristic_by_uuid::<[u8; SPLIT_MESSAGE_MAX_SIZE]>(
                service,
                // uuid: 4b3514fb-cae4-4d38-a097-3a2a3d1c3b9c
                &Uuid::Uuid128([
                    156u8, 59u8, 28u8, 61u8, 42u8, 58u8, 151u8, 160u8, 56u8, 77u8, 228u8, 202u8, 251u8, 20u8, 53u8,
                    75u8,
                ]),
            )
            .await?;
        info!("Subscribing notifications");
        let listener = client.subscribe(&message_to_central, false).await?;
        let mut split_ble_driver = BleSplitCentralDriver::new(listener, message_to_peripheral, client);
        if !validate_split_product(&mut split_ble_driver).await {
            warn!("Rejecting split peripheral {} after product validation", id);
            return Ok(());
        }
        peer_validated.set(true);
        commit_peer_candidate(id, peer_address).await;
        publish_peripheral_connection(id, true);

        let peripheral_manager = PeripheralManager::<ROW, COL, ROW_OFFSET, COL_OFFSET, _>::new(split_ble_driver, id);
        peripheral_manager.run().await;
        info!("Peripheral manager stopped");
    };
    Ok(())
}

/// Ble central driver which reads and writes the split message.
///
/// Different from serial, BLE split message is processed in a separate service.
/// The BLE service should keep running, it processes the split message in the callback, which is not async.
/// It's impossible to implement `SplitReader` or `SplitWriter` for BLE service,
/// so we need this wrapper to forward split message to channel.
pub(crate) struct BleSplitCentralDriver<'a, 'b, 'c, C: Controller + ControllerCmdAsync<LeSetPhy>, P: PacketPool> {
    // Listener for split message from peripheral
    listener: NotificationListener<'b, 512>,
    // Characteristic to send split message to peripheral
    message_to_peripheral: Characteristic<[u8; SPLIT_MESSAGE_MAX_SIZE]>,
    // Client
    client: &'c GattClient<'a, C, P, 10>,
}

impl<'a, 'b, 'c, C: Controller + ControllerCmdAsync<LeSetPhy>, P: PacketPool> BleSplitCentralDriver<'a, 'b, 'c, C, P> {
    pub(crate) fn new(
        listener: NotificationListener<'b, 512>,
        message_to_peripheral: Characteristic<[u8; SPLIT_MESSAGE_MAX_SIZE]>,
        client: &'c GattClient<'a, C, P, 10>,
    ) -> Self {
        Self {
            listener,
            message_to_peripheral,
            client,
        }
    }
}

impl<'a, 'b, 'c, C: Controller + ControllerCmdAsync<LeSetPhy>, P: PacketPool> SplitReader
    for BleSplitCentralDriver<'a, 'b, 'c, C, P>
{
    async fn read(&mut self) -> Result<SplitMessage, SplitDriverError> {
        let data = self.listener.next().await;
        let message = postcard::from_bytes(data.as_ref()).map_err(|_| SplitDriverError::DeserializeError)?;
        info!("Received split message: {:?}", message);

        match &message {
            SplitMessage::Pointing(_) => {
                debug!("Pointing activity {:?} detected from peripheral", &message);
                update_pointing_activity_time();
            }
            SplitMessage::Key(_) => {
                debug!("Key activity {:?} detected from peripheral", &message);
                update_activity_time();
            }
            _ => {}
        }

        Ok(message)
    }
}

impl<'a, 'b, 'c, C: Controller + ControllerCmdAsync<LeSetPhy>, P: PacketPool> SplitWriter
    for BleSplitCentralDriver<'a, 'b, 'c, C, P>
{
    async fn write(&mut self, message: &SplitMessage) -> Result<usize, SplitDriverError> {
        let mut buf = [0_u8; SPLIT_MESSAGE_MAX_SIZE];
        match postcard::to_slice(&message, &mut buf) {
            Ok(_bytes) => {
                if let Err(e) = self
                    .client
                    .write_characteristic_without_response(&self.message_to_peripheral, &buf)
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
            }
            Err(e) => error!("Postcard serialize split message error: {}", e),
        };

        Ok(SPLIT_MESSAGE_MAX_SIZE)
    }
}

/// Wait for the BLE stack to start.
///
/// If the BLE stack has been started, wait 500ms then quit.
pub(crate) async fn wait_for_stack_started() {
    loop {
        if STACK_STARTED.signaled() {
            embassy_time::Timer::after_millis(500).await;
            break;
        }
        embassy_time::Timer::after_millis(500).await;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SplitPowerMode {
    Pointing,
    Active,
    Idle,
    Sleeping,
}

fn desired_split_power_mode(
    now_ms: u32,
    last_activity_ms: u32,
    last_pointing_activity_ms: u32,
    sleep_requested: bool,
) -> SplitPowerMode {
    let inactive_ms = now_ms.wrapping_sub(last_activity_ms);
    if sleep_requested
        || (SPLIT_CENTRAL_SLEEP_TIMEOUT_SECONDS != 0
            && inactive_ms >= u32::from(SPLIT_CENTRAL_SLEEP_TIMEOUT_SECONDS).saturating_mul(1_000))
    {
        SplitPowerMode::Sleeping
    } else if last_pointing_activity_ms != 0
        && now_ms.wrapping_sub(last_pointing_activity_ms) < SPLIT_POINTING_ACTIVE_WINDOW_MS
    {
        SplitPowerMode::Pointing
    } else if inactive_ms >= SPLIT_ACTIVE_WINDOW_MS {
        SplitPowerMode::Idle
    } else {
        SplitPowerMode::Active
    }
}

/// Own the split central's global sleep state.
///
/// This task is independent of individual peripheral connections so the
/// configured inactivity timeout still publishes `SleepStateEvent` when a
/// link is missing, reconnecting, or changing its connection parameters.
pub async fn run_split_power_state_manager() -> ! {
    let now_ms = Instant::now().as_millis() as u32;
    if LAST_ACTIVITY_MS
        .compare_exchange(0, now_ms, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        SPLIT_SLEEP_REQUESTED.store(false, Ordering::Release);
    }

    let mut sleeping = desired_split_power_mode(
        now_ms,
        LAST_ACTIVITY_MS.load(Ordering::Acquire),
        LAST_POINTING_ACTIVITY_MS.load(Ordering::Acquire),
        SPLIT_SLEEP_REQUESTED.load(Ordering::Acquire),
    ) == SplitPowerMode::Sleeping;

    if sleeping && !SLEEPING_STATE.swap(true, Ordering::AcqRel) {
        publish_event(SleepStateEvent::new(true));
    }

    let mut activity = power_activity_receiver();
    loop {
        wait_for_power_reevaluation(&mut activity).await;
        let next_sleeping = desired_split_power_mode(
            Instant::now().as_millis() as u32,
            LAST_ACTIVITY_MS.load(Ordering::Acquire),
            LAST_POINTING_ACTIVITY_MS.load(Ordering::Acquire),
            SPLIT_SLEEP_REQUESTED.load(Ordering::Acquire),
        ) == SplitPowerMode::Sleeping;
        if next_sleeping == sleeping {
            continue;
        }

        sleeping = next_sleeping;
        if SLEEPING_STATE.swap(sleeping, Ordering::AcqRel) != sleeping {
            publish_event(SleepStateEvent::new(sleeping));
        }
    }
}

/// Adapt split-link connection parameters to recent keyboard activity.
///
/// State is shared through atomics instead of a single-consumer signal so a
/// Qube can manage both peripheral links independently.
async fn sleep_manager_task<
    'b,
    's: 'b,
    C: Controller + ControllerCmdAsync<LeSetPhy> + ControllerCmdSync<LeReadLocalSupportedFeatures>,
    P: PacketPool,
>(
    stack: &'b Stack<'s, C, P>,
    conn: &Connection<'b, P>,
) -> Result<(), BleHostError<C::Error>> {
    info!(
        "Adaptive split power manager started with {}s sleep timeout",
        SPLIT_CENTRAL_SLEEP_TIMEOUT_SECONDS
    );

    let mut current_mode = SplitPowerMode::Active;
    let mut activity = power_activity_receiver();
    loop {
        wait_for_power_reevaluation(&mut activity).await;
        let next_mode = desired_split_power_mode(
            Instant::now().as_millis() as u32,
            LAST_ACTIVITY_MS.load(Ordering::Acquire),
            LAST_POINTING_ACTIVITY_MS.load(Ordering::Acquire),
            SPLIT_SLEEP_REQUESTED.load(Ordering::Acquire),
        );
        if next_mode == current_mode {
            continue;
        }

        let conn_params = match next_mode {
            SplitPowerMode::Pointing => {
                info!("Split link entering pointing mode");
                pointing_central_conn_param()
            }
            SplitPowerMode::Active => {
                info!("Split link entering active mode");
                defaul_central_conn_param()
            }
            SplitPowerMode::Idle => {
                info!("Split link entering idle mode");
                idle_central_conn_param()
            }
            SplitPowerMode::Sleeping => {
                info!("Split link entering sleep mode");
                sleeping_central_conn_param()
            }
        };
        update_conn_params(stack, conn, &conn_params).await;

        // A single split link owns the central sleep state itself. Keeping the
        // transition here avoids an extra global task on ordinary two-half
        // keyboards, while multi-peripheral centrals (for example Qube) use
        // `run_split_power_state_manager` so one link cannot consume another
        // link's sleep transition.
        if crate::SPLIT_PERIPHERALS_NUM == 1 {
            if next_mode == SplitPowerMode::Sleeping {
                if !SLEEPING_STATE.swap(true, Ordering::AcqRel) {
                    publish_event(SleepStateEvent::new(true));
                }
            } else if current_mode == SplitPowerMode::Sleeping && SLEEPING_STATE.swap(false, Ordering::AcqRel) {
                publish_event(SleepStateEvent::new(false));
            }
        }
        current_mode = next_mode;
    }
}

/// Update the activity time to indicate user activity
pub(crate) fn update_activity_time() {
    let now_ms = Instant::now().as_millis() as u32;
    LAST_ACTIVITY_MS.store(now_ms, Ordering::Release);
    SPLIT_SLEEP_REQUESTED.store(false, Ordering::Release);
    SPLIT_ACTIVITY_WATCH.sender().send(now_ms);
    debug!("Activity detected, restoring active split link");
}

/// Record pointing motion so the split link can temporarily use a 7.5 ms interval.
pub(crate) fn update_pointing_activity_time() {
    let now_ms = Instant::now().as_millis() as u32;
    LAST_POINTING_ACTIVITY_MS.store(now_ms, Ordering::Release);
    LAST_ACTIVITY_MS.store(now_ms, Ordering::Release);
    SPLIT_SLEEP_REQUESTED.store(false, Ordering::Release);
    SPLIT_ACTIVITY_WATCH.sender().send(now_ms);
    debug!("Pointing activity detected, restoring low-latency split link");
}

/// Subscribe to activity notifications with whatever is already recorded marked
/// as seen.
///
/// A fresh `Watch` receiver starts before every past update, so its first wait
/// would return instantly and re-evaluate the power mode while the link is
/// still being brought up — renegotiating connection parameters right as the
/// peripheral's settings snapshot is being pushed, which drops it.
fn power_activity_receiver() -> Option<Receiver<'static, crate::RawMutex, u32, SPLIT_POWER_WATCHERS>> {
    let mut receiver = SPLIT_ACTIVITY_WATCH.receiver();
    if let Some(receiver) = receiver.as_mut() {
        let _ = receiver.try_changed();
    }
    receiver
}

/// Wait until the split power mode is worth re-evaluating: either the poll
/// interval elapsed (activity aging into idle/sleep) or activity was reported.
async fn wait_for_power_reevaluation(activity: &mut Option<Receiver<'_, crate::RawMutex, u32, SPLIT_POWER_WATCHERS>>) {
    match activity {
        Some(activity) => {
            select(Timer::after_millis(SPLIT_POWER_POLL_MS), activity.changed()).await;
        }
        // More watchers than slots should be impossible, but falling back to
        // plain polling keeps the link manageable instead of panicking.
        None => Timer::after_millis(SPLIT_POWER_POLL_MS).await,
    }
}

/// Request deep split-link sleep from a host suspend or transport timeout.
pub(crate) fn request_sleep() {
    SPLIT_SLEEP_REQUESTED.store(true, Ordering::Release);
}

#[cfg(test)]
mod advertisement_tests {
    use super::*;

    fn current_advertisement(product_id: u16, peripheral_id: u8) -> [u8; 28] {
        let mut data = [0u8; 28];
        data[0..3].copy_from_slice(&[2, 0x01, LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED]);
        data[3] = 17;
        data[4] = 0x07;
        data[5..21].copy_from_slice(&SPLIT_SERVICE_UUID);
        data[21..28].copy_from_slice(&[
            6,
            0xff,
            (SPLIT_COMPANY_ID & 0xff) as u8,
            (SPLIT_COMPANY_ID >> 8) as u8,
            (product_id & 0xff) as u8,
            (product_id >> 8) as u8,
            peripheral_id,
        ]);
        data
    }

    #[test]
    fn current_advertisement_requires_matching_product() {
        let matching = current_advertisement(crate::SPLIT_PRODUCT_ID, 1);
        assert_eq!(split_peripheral_id_from_advertisement(&matching), Some(1));

        let mismatched = current_advertisement(crate::SPLIT_PRODUCT_ID.wrapping_add(1), 1);
        assert_eq!(split_peripheral_id_from_advertisement(&mismatched), None);
    }

    #[test]
    fn current_advertisement_requires_split_service() {
        let mut data = current_advertisement(crate::SPLIT_PRODUCT_ID, 0);
        data[4] = 0x06;
        assert_eq!(split_peripheral_id_from_advertisement(&data), None);
    }

    #[test]
    fn legacy_advertisement_remains_discoverable_during_migration() {
        let mut data = [0u8; 26];
        data[0..3].copy_from_slice(&[2, 0x01, LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED]);
        data[3] = 17;
        data[4] = 0x07;
        data[5..21].copy_from_slice(&SPLIT_SERVICE_UUID);
        data[21..26].copy_from_slice(&[4, 0xff, 0x18, 0xe1, 1]);

        assert_eq!(legacy_split_peripheral_id_from_advertisement(&data), Some(1));
    }

    #[test]
    fn split_power_mode_tracks_activity_and_sleep_timeout() {
        assert_eq!(desired_split_power_mode(1_999, 0, 0, false), SplitPowerMode::Active);
        assert_eq!(desired_split_power_mode(2_000, 0, 0, false), SplitPowerMode::Idle);
        assert_eq!(
            desired_split_power_mode(2_100, 2_000, 2_000, false),
            SplitPowerMode::Pointing
        );
        assert_eq!(
            desired_split_power_mode(2_500, 2_000, 2_000, false),
            SplitPowerMode::Active
        );

        if SPLIT_CENTRAL_SLEEP_TIMEOUT_SECONDS != 0 {
            let timeout_ms = u32::from(SPLIT_CENTRAL_SLEEP_TIMEOUT_SECONDS) * 1_000;
            assert_eq!(
                desired_split_power_mode(timeout_ms, 0, 0, false),
                SplitPowerMode::Sleeping
            );
        }
        assert_eq!(desired_split_power_mode(1, 0, 0, true), SplitPowerMode::Sleeping);
    }

    #[test]
    fn pointing_split_link_uses_7_5_ms_without_slave_latency() {
        let params = pointing_central_conn_param();

        assert_eq!(params.min_connection_interval, Duration::from_micros(7_500));
        assert_eq!(params.max_connection_interval, Duration::from_micros(7_500));
        assert_eq!(params.max_latency, 0);
    }

    #[test]
    fn sleeping_split_link_bounds_first_peripheral_event_latency() {
        let params = sleeping_central_conn_param();

        assert_eq!(params.min_connection_interval, Duration::from_millis(100));
        assert_eq!(params.max_connection_interval, Duration::from_millis(100));
        assert_eq!(params.max_latency, 0);
    }
}
