use core::cell::{Cell, RefCell};
use core::sync::atomic::Ordering;

use bt_hci::cmd::le::{LeReadLocalSupportedFeatures, LeSetPhy, LeSetScanParams};
use bt_hci::controller::{ControllerCmdAsync, ControllerCmdSync};
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer, with_timeout};
use heapless::VecView;
use trouble_host::prelude::*;

use crate::SPLIT_CENTRAL_SLEEP_TIMEOUT_SECONDS;
use crate::ble::{SLEEPING_STATE, update_ble_phy, update_conn_params};
use crate::channel::FLASH_CHANNEL;
use crate::event::{PeripheralConnectedEvent, SleepStateEvent, publish_event};
#[cfg(feature = "storage")]
use crate::split::ble::PeerAddress;
use crate::split::driver::{PeripheralManager, SplitDriverError, SplitReader, SplitWriter};
use crate::split::{SPLIT_MESSAGE_MAX_SIZE, SplitMessage};
use crate::storage::FlashOperationMessage;

pub(crate) static STACK_STARTED: Signal<crate::RawMutex, bool> = Signal::new();
pub(crate) static PERIPHERAL_FOUND: Signal<crate::RawMutex, (u8, BdAddr)> = Signal::new();

// Signals and mutex for syncing scanning state between scanning task and peripheral manager
static START_SCANNING: Signal<crate::RawMutex, ()> = Signal::new();
static STOP_SCANNING: Signal<crate::RawMutex, ()> = Signal::new();
static SCANNING_MUTEX: Mutex<crate::RawMutex, ()> = Mutex::new(());
static UNCOMMITTED_PEER_CANDIDATES: BlockingMutex<crate::RawMutex, Cell<u32>> = BlockingMutex::new(Cell::new(0));

/// Sleep management signal for BLE Split Central
///
/// This signal serves dual purposes for sleep management:
/// - `signal(true)`: Indicates central has entered sleep mode
/// - `signal(false)`: Indicates activity detected, wake up or reset sleep timer
pub(crate) static CENTRAL_SLEEP: Signal<crate::RawMutex, bool> = Signal::new();

const SPLIT_SERVICE_UUID: [u8; 16] = [70, 153, 101, 152, 54, 53, 10, 191, 7, 75, 229, 24, 170, 251, 213, 77];
const SPLIT_COMPANY_ID: u16 = 0xe118;

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

pub async fn scan_common_peripherals<
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
        START_SCANNING.wait().await;
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
                        info!("Start common split peripheral scan");
                        STOP_SCANNING.wait().await;
                        info!("Stop common split peripheral scan");
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

                    info!("Scanned new common split peripheral {:?}", scanned_addr);
                    let mut slot_updated = false;
                    if let Some(slot) = addrs.borrow_mut().get_mut(found_peripheral_id as usize)
                        && slot.is_none()
                    {
                        *slot = Some(scanned_addr);
                        slot_updated = true;
                    }

                    if slot_updated {
                        mark_uncommitted_peer_candidate(found_peripheral_id as usize);
                    }

                    if addrs.borrow().iter().all(|a| a.is_some()) {
                        break;
                    }
                }
            };

            select(scanning_fut, update_addrs_fut).await;
        }
    }
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

                    info!("Scanned new peripheral {:?}", scanned_addr);
                    let mut slot_updated = false;
                    if let Some(slot) = addrs.borrow_mut().get_mut(found_peripheral_id as usize)
                        && slot.is_none()
                    {
                        // Update only when the slot is empty
                        *slot = Some(scanned_addr);
                        slot_updated = true;
                    }

                    // Update stored addr.
                    // This cannot be put inside the `addrs.borrow_mut()` block because the sending is async
                    if slot_updated {
                        FLASH_CHANNEL
                            .send(FlashOperationMessage::PeerAddress(PeerAddress::new(
                                found_peripheral_id,
                                true,
                                scanned_addr,
                            )))
                            .await;
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
            if let Some(peripheral_id) = common_split_peripheral_id_from_advertisement(report.data).or_else(|| {
                // Backward-compatible Qube/root advertisement format.
                if report.data.len() > 25
                    && report.data[4] == 0x07
                    && report.data[5..].starts_with(&SPLIT_SERVICE_UUID)
                    && report.data[21..25] == [0x04, 0xff, 0x18, 0xe1]
                {
                    Some(report.data[25])
                } else {
                    None
                }
            }) {
                info!("Found split peripheral: id={:?}, addr={:?}", peripheral_id, report.addr);
                PERIPHERAL_FOUND.signal((peripheral_id, report.addr));
                break;
            }
        }
    }
}

fn common_split_peripheral_id_from_advertisement(data: &[u8]) -> Option<u8> {
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

pub async fn run_common_ble_peripheral_manager<
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
        let address = loop {
            if let Some(Some(addr)) = addrs.borrow().get(peri_id) {
                break Address::random(*addr);
            }
            if !START_SCANNING.signaled() {
                START_SCANNING.signal(());
            }
            embassy_time::Timer::after_millis(500).await;
        };
        info!("Common peripheral peer address: {:?}", address);

        let mut central = stack.central();
        let config = ConnectConfig {
            connect_params: defaul_central_conn_param(),
            scan_config: ScanConfig {
                filter_accept_list: &[address],
                ..Default::default()
            },
        };
        wait_for_stack_started().await;

        publish_event(PeripheralConnectedEvent {
            id: peri_id,
            connected: false,
        });

        match with_timeout(Duration::from_secs(5), async {
            if let Ok(_guard) = SCANNING_MUTEX.try_lock() {
                info!("Start connecting to common peripheral {}", peri_id);
                central.connect(&config).await
            } else {
                STOP_SCANNING.signal(());
                let _guard = SCANNING_MUTEX.lock().await;
                embassy_time::Timer::after_millis(100).await;
                info!("Start connecting to common peripheral {}", peri_id);
                central.connect(&config).await
            }
        })
        .await
        {
            Ok(Ok(conn)) => {
                info!("Connected to common peripheral {}", peri_id);

                publish_event(PeripheralConnectedEvent {
                    id: peri_id,
                    connected: true,
                });

                match run_common_central_manager_task::<_, _, ROW, COL, ROW_OFFSET, COL_OFFSET>(
                    peri_id,
                    address.addr.into_inner(),
                    stack,
                    &conn,
                )
                .await
                {
                    Ok(true) => clear_uncommitted_peer_candidate(peri_id),
                    Ok(false) => {
                        warn!("Common peripheral {} product check failed", peri_id);
                        drop_uncommitted_peer_candidate(peri_id, addrs);
                    }
                    Err(e) => {
                        #[cfg(feature = "defmt")]
                        let e = defmt::Debug2Format(&e);
                        error!("Common BLE central error: {:?}", e);
                        drop_uncommitted_peer_candidate(peri_id, addrs);
                    }
                }
            }
            Ok(Err(e)) => {
                #[cfg(feature = "defmt")]
                let e = defmt::Debug2Format(&e);
                error!("Connect to common peripheral {} error: {:?}", peri_id, e);
                drop_uncommitted_peer_candidate(peri_id, addrs);
            }
            Err(_) => {
                warn!("Connect to common peripheral {} timeout", peri_id);
                drop_uncommitted_peer_candidate(peri_id, addrs);
            }
        }
        embassy_time::Timer::after_millis(500).await;
    }
}

fn bit_for_peri(peri_id: usize) -> u32 {
    1u32 << peri_id.min(31)
}

fn mark_uncommitted_peer_candidate(peri_id: usize) {
    let bit = bit_for_peri(peri_id);
    UNCOMMITTED_PEER_CANDIDATES.lock(|cell| cell.set(cell.get() | bit));
}

fn clear_uncommitted_peer_candidate(peri_id: usize) {
    let bit = bit_for_peri(peri_id);
    UNCOMMITTED_PEER_CANDIDATES.lock(|cell| cell.set(cell.get() & !bit));
}

fn take_uncommitted_peer_candidate(peri_id: usize) -> bool {
    let bit = bit_for_peri(peri_id);
    UNCOMMITTED_PEER_CANDIDATES.lock(|cell| {
        let current = cell.get();
        cell.set(current & !bit);
        current & bit != 0
    })
}

fn drop_uncommitted_peer_candidate(peri_id: usize, addrs: &RefCell<VecView<Option<[u8; 6]>>>) {
    if take_uncommitted_peer_candidate(peri_id)
        && let Some(addr) = addrs.borrow_mut().get_mut(peri_id)
    {
        *addr = None;
    }
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
                ..Default::default()
            },
        };
        wait_for_stack_started().await;

        publish_event(PeripheralConnectedEvent {
            id: peri_id,
            connected: false,
        });

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

                publish_event(PeripheralConnectedEvent {
                    id: peri_id,
                    connected: true,
                });

                if let Err(e) =
                    run_central_manager_task::<_, _, ROW, COL, ROW_OFFSET, COL_OFFSET>(peri_id, stack, &conn).await
                {
                    #[cfg(feature = "defmt")]
                    let e = defmt::Debug2Format(&e);
                    error!("BLE central error: {:?}", e);
                }
            }
            Ok(Err(e)) => {
                #[cfg(feature = "defmt")]
                let e = defmt::Debug2Format(&e);
                error!("Connect to peripheral {} error: {:?}", peri_id, e);
            }
            Err(_) => {
                // Connect to peripheral timeout
                warn!("Connect to peripheral {} timeout, clearing", peri_id);
                if let Some(addr) = addrs.borrow_mut().get_mut(peri_id) {
                    *addr = None
                };
            }
        }
        // Reconnect after 500ms
        embassy_time::Timer::after_millis(500).await;
    }
}

fn defaul_central_conn_param() -> RequestedConnParams {
    RequestedConnParams {
        min_connection_interval: Duration::from_micros(7500),
        max_connection_interval: Duration::from_micros(7500),
        // Keep active split links awake every interval so central-to-peripheral
        // layer/state updates reach LEDs without slave-latency delay.
        max_latency: 0,
        supervision_timeout: Duration::from_secs(5),
        ..Default::default()
    }
}

async fn run_common_central_manager_task<
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
) -> Result<bool, BleHostError<C::Error>> {
    let client = GattClient::<C, P, 10>::new(stack, conn).await?;

    update_ble_phy(stack, conn).await;

    info!("Updating common split connection parameters for peripheral");
    update_conn_params(stack, conn, &defaul_central_conn_param()).await;

    match select3(
        ble_central_task(&client, conn),
        run_common_peripheral_manager::<_, _, ROW, COL, ROW_OFFSET, COL_OFFSET>(id, peer_address, &client),
        sleep_manager_task(stack, conn),
    )
    .await
    {
        Either3::First(e) => e.map(|_| true),
        Either3::Second(e) => e,
        Either3::Third(e) => e.map(|_| true),
    }
}

async fn run_common_peripheral_manager<
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
) -> Result<bool, BleHostError<C::Error>> {
    let services = client.services_by_uuid(&Uuid::new_long(SPLIT_SERVICE_UUID)).await?;
    info!("Common split services found");
    if let Some(service) = services.first() {
        let message_to_central = client
            .characteristic_by_uuid::<[u8; SPLIT_MESSAGE_MAX_SIZE]>(
                service,
                &Uuid::Uuid128([
                    195u8, 139u8, 18u8, 232u8, 162u8, 55u8, 46u8, 141u8, 194u8, 69u8, 11u8, 189u8, 227u8, 19u8, 99u8,
                    14u8,
                ]),
            )
            .await?;
        info!("Common message to central found");
        let message_to_peripheral = client
            .characteristic_by_uuid::<[u8; SPLIT_MESSAGE_MAX_SIZE]>(
                service,
                &Uuid::Uuid128([
                    156u8, 59u8, 28u8, 61u8, 42u8, 58u8, 151u8, 160u8, 56u8, 77u8, 228u8, 202u8, 251u8, 20u8, 53u8,
                    75u8,
                ]),
            )
            .await?;
        info!("Subscribing common split notifications");
        let listener = client.subscribe(&message_to_central, false).await?;
        let mut split_ble_driver = BleSplitCentralDriver::new(listener, message_to_peripheral, client);
        if !validate_split_product(&mut split_ble_driver).await {
            return Ok(false);
        }

        #[cfg(feature = "storage")]
        FLASH_CHANNEL
            .send(FlashOperationMessage::PeerAddress(PeerAddress::new(
                id as u8,
                true,
                peer_address,
            )))
            .await;

        let peripheral_manager = PeripheralManager::<ROW, COL, ROW_OFFSET, COL_OFFSET, _>::new(split_ble_driver, id);
        peripheral_manager.run().await;
        info!("Common peripheral manager stopped");
        return Ok(true);
    };
    Ok(false)
}

async fn validate_split_product<T: SplitReader + SplitWriter>(driver: &mut T) -> bool {
    if let Err(e) = driver.write(&SplitMessage::ProductId(crate::SPLIT_PRODUCT_ID)).await {
        warn!("Common split product check write failed: {:?}", e);
        return false;
    }

    match with_timeout(Duration::from_millis(1500), driver.read()).await {
        Ok(Ok(SplitMessage::ProductId(product_id))) if product_id == crate::SPLIT_PRODUCT_ID => true,
        Ok(Ok(SplitMessage::ProductId(product_id))) => {
            warn!(
                "Common split product id mismatch: got {}, expected {}",
                product_id,
                crate::SPLIT_PRODUCT_ID
            );
            false
        }
        Ok(Ok(message)) => {
            warn!("Unexpected common split product check response: {:?}", message);
            false
        }
        Ok(Err(e)) => {
            warn!("Common split product check read failed: {:?}", e);
            false
        }
        Err(_) => {
            warn!("Common split product check timeout");
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
    stack: &'b Stack<'s, C, P>,
    conn: &Connection<'b, P>,
) -> Result<(), BleHostError<C::Error>> {
    let client = GattClient::<C, P, 10>::new(stack, conn).await?;

    // Use 2M Phy
    update_ble_phy(stack, conn).await;

    info!("Updating connection parameters for peripheral");
    update_conn_params(stack, conn, &defaul_central_conn_param()).await;

    match select3(
        ble_central_task(&client, conn),
        run_peripheral_manager::<_, _, ROW, COL, ROW_OFFSET, COL_OFFSET>(id, &client),
        sleep_manager_task(stack, conn),
    )
    .await
    {
        Either3::First(e) => e,
        Either3::Second(e) => e,
        Either3::Third(e) => e,
    }
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
    client: &GattClient<'a, C, P, 10>,
) -> Result<(), BleHostError<C::Error>> {
    let services = client
        .services_by_uuid(&Uuid::new_long([
            70u8, 153u8, 101u8, 152u8, 54u8, 53u8, 10u8, 191u8, 7u8, 75u8, 229u8, 24u8, 170u8, 251u8, 213u8, 77u8,
        ]))
        .await?;
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
        let split_ble_driver = BleSplitCentralDriver::new(listener, message_to_peripheral, client);
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

        // Update last activity time when receiving key events from peripheral
        if matches!(message, SplitMessage::Key(_) | SplitMessage::Pointing(_)) {
            debug!("Activity {:?} detected from peripheral", &message);
            update_activity_time();
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

/// Sleep manager task for connection between split central and peripheral
/// Handles sleep timeout and connection parameter adjustments using event-driven approach
async fn sleep_manager_task<
    'b,
    's: 'b,
    C: Controller + ControllerCmdAsync<LeSetPhy> + ControllerCmdSync<LeReadLocalSupportedFeatures>,
    P: PacketPool,
>(
    stack: &'b Stack<'s, C, P>,
    conn: &Connection<'b, P>,
) -> Result<(), BleHostError<C::Error>> {
    // Skip sleep management if timeout is 0 (disabled)
    if SPLIT_CENTRAL_SLEEP_TIMEOUT_SECONDS == 0 {
        info!("Sleep management disabled (timeout = 0)");
        core::future::pending::<()>().await;
        return Ok(());
    }

    info!(
        "Sleep manager started with {}s timeout",
        SPLIT_CENTRAL_SLEEP_TIMEOUT_SECONDS
    );

    loop {
        if !SLEEPING_STATE.load(Ordering::Acquire) {
            // Wait for timeout or activity (false signal means activity/wakeup)
            match select(
                Timer::after_secs(SPLIT_CENTRAL_SLEEP_TIMEOUT_SECONDS.into()),
                CENTRAL_SLEEP.wait(),
            )
            .await
            {
                Either::First(_) => {
                    // Timeout: enter sleep mode
                }
                Either::Second(signal_value) => {
                    // Received signal - if false, it means activity detected
                    if !signal_value {
                        debug!("Activity detected, resetting sleep timeout");
                        continue;
                    }
                    // True, enter sleep mode
                }
            }

            // Timeout or received true from CENTRAL_SLEEP signal, enter sleep mode
            info!("Entering sleep mode");

            // `conn` is the split central -> peripheral BLE link. While the
            // central is sleeping, use a longer interval to reduce central-side
            // radio wakeups; normal params are restored on activity.
            let conn_params = RequestedConnParams {
                min_connection_interval: Duration::from_millis(200),
                max_connection_interval: Duration::from_millis(200),
                max_latency: 25, // 5s
                supervision_timeout: Duration::from_secs(11),
                ..Default::default()
            };

            // Update connection parameters
            update_conn_params(stack, conn, &conn_params).await;
            SLEEPING_STATE.store(true, Ordering::Release);

            publish_event(SleepStateEvent::new(true));
        } else {
            // Wait for activity to wake up (false signal means activity/wakeup)
            let signal_value = CENTRAL_SLEEP.wait().await;
            if !signal_value {
                info!("Waking up from sleep mode due to activity");
                SLEEPING_STATE.store(false, Ordering::Release);

                publish_event(SleepStateEvent::new(false));

                // Restore normal connection parameters
                update_conn_params(stack, conn, &defaul_central_conn_param()).await;
            }
        }
    }
}

/// Update the activity time to indicate user activity
/// This function triggers activity wakeup signal for sleep management
pub(crate) fn update_activity_time() {
    CENTRAL_SLEEP.signal(false);
    debug!("Activity detected, signaling wakeup");
}
